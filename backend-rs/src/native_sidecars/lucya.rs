//! Lucya (app.lucya.com), life insurance and PER: signs in with the identifier and the password
//! and reads the contracts the home page lists, each with its value.
//!
//! The site is a server-rendered form login: the page hands out a `_csrf_token`, the form is
//! posted with the session cookie, and a successful login lands on the home page. Nothing is kept
//! once the contracts are read, the sign-in is never retried and the site's logout is never
//! called. An account asked for a second factor or a captcha is refused with its own code.
//!
//! A refusal is logged with its step, a status and a size only: never the identifier, a token or
//! anything a page says.

use std::collections::HashSet;
use std::sync::LazyLock;
use std::time::Duration;

use chrono::NaiveDate;
use http::{Method, StatusCode};
use regex::Regex;
use rust_decimal::Decimal;
use url::Url;

use super::NativeSidecarError;
use super::common::{
    MAX_RESPONSE_BYTES, MemoryCookieJar, build_http_client, normalize_base_url, read_capped_text,
    same_origin_url, unescape_then_strip_html_text,
};

pub const DEFAULT_BASE_URL: &str = "https://app.lucya.com";
pub const LOGIN_PATH: &str = "/asv/espace-client/connexion";

const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
    (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_REDIRECTS: usize = 5;

/// Plain HTTP to a loopback address, with no credentials, query or fragment. A copy of the Natixis
/// connector's private check, so that no file shared with upstream changes.
fn is_local_http(url: &str) -> bool {
    let Ok(parsed) = Url::parse(url) else {
        return false;
    };
    let loopback = match parsed.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(_)) | None => false,
    };
    loopback
        && parsed.scheme() == "http"
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && parsed.query().is_none()
        && parsed.fragment().is_none()
}

/// Where to sign in: the real site, or a stub on this machine. Any other setting is ignored, so
/// that none can send the password elsewhere.
pub fn base_url(configured: &str) -> &str {
    if is_local_http(configured) {
        return configured;
    }
    if !configured.is_empty() {
        tracing::warn!("LUCYA_BASE_URL is ignored: it needs a loopback http URL");
    }
    DEFAULT_BASE_URL
}

macro_rules! regex {
    ($name:ident, $pattern:expr) => {
        static $name: LazyLock<Regex> =
            LazyLock::new(|| Regex::new($pattern).expect("valid pattern"));
    };
}

regex!(INPUT, r"(?is)<input\b[^>]*>");
regex!(VALUE, r#"(?i)\bvalue\s*=\s*"([^"]*)""#);
regex!(LOGIN_FORM, r#"(?is)<form\b[^>]*\bid\s*=\s*"login""#);
regex!(
    HOME,
    r#"(?is)<body\b[^>]*\bclass\s*=\s*"[^"]*\blayout-synthesis\b"#
);
regex!(CAPTCHA, r"(?i)g-recaptcha|grecaptcha|recaptcha/api");
regex!(
    CONTRACT,
    r#"(?is)<article\b[^>]*\bclass\s*=\s*"(?:[^"]*\s)?contract(?:\s[^"]*)?"[^>]*>(.*?)</article>"#
);
regex!(
    KIND,
    r#"(?is)<span\b[^>]*\blabel--monochrome-secondary\b[^>]*>(.*?)</span>"#
);
regex!(
    NUMBER,
    r#"(?is)<span\b[^>]*\blabel--primary\b[^>]*>.*?<strong\b[^>]*>(.*?)</strong>"#
);
regex!(INSURER, r#"(?is)<img\b[^>]*\balt\s*=\s*"([^"]*)""#);
regex!(
    NAME,
    r#"(?is)<h3\b[^>]*\bcontract__name\b[^>]*>\s*<a\b[^>]*\bhref\s*=\s*"[^"]*/asv/espace-client/contrat/([0-9]{1,18})"[^>]*>(.*?)</a>"#
);
regex!(
    PRICE,
    r#"(?is)<span\b[^>]*\bcontract__price\b[^>]*>(.*?)</span>"#
);
regex!(
    VALUED_ON,
    r#"(?is)<time\b[^>]*\bcontract__date\b[^>]*\bdatetime\s*=\s*"([^"]*)""#
);
regex!(
    TOTAL,
    r#"(?is)<article\b[^>]*\bcard--important\b[^>]*>.*?<h2\b[^>]*\bcard__title\b[^>]*>(.*?)</h2>"#
);

/// One contract of the home page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contract {
    /// The site's own identifier, from the link to the contract page.
    pub id: String,
    /// The contract number the page prints.
    pub number: Option<String>,
    pub name: String,
    /// The page's wording for the envelope, such as "Assurance vie" or "PER".
    pub kind: String,
    pub insurer: Option<String>,
    pub value_eur: Decimal,
    pub valued_on: NaiveDate,
}

fn refused(status: StatusCode, code: &str) -> NativeSidecarError {
    NativeSidecarError::new(status, code)
}

fn changed(step: &str, why: &str) -> NativeSidecarError {
    tracing::warn!("Lucya read stopped at the {step}: {why}");
    refused(StatusCode::BAD_GATEWAY, "UPSTREAM_FORMAT_CHANGED")
}

fn unavailable(step: &str, why: &str) -> NativeSidecarError {
    tracing::warn!("Lucya read stopped at the {step}: {why}");
    refused(StatusCode::BAD_GATEWAY, "UPSTREAM_UNAVAILABLE")
}

/// Where a request ended, once its redirects on the same site were followed.
struct Page {
    path: String,
    body: String,
}

async fn send(
    client: &reqwest::Client,
    base: &Url,
    step: &str,
    form: Option<&[(&str, &str)]>,
) -> Result<Page, NativeSidecarError> {
    let mut url = base
        .join(LOGIN_PATH)
        .map_err(|_| unavailable(step, "the address is not a URL"))?;
    let mut method = if form.is_some() {
        Method::POST
    } else {
        Method::GET
    };
    for _ in 0..=MAX_REDIRECTS {
        let mut request = client.request(method.clone(), url.clone());
        if method == Method::POST
            && let Some(fields) = form
        {
            request = request.form(fields);
        }
        let response = request
            .send()
            .await
            .map_err(|_| unavailable(step, "the site could not be reached"))?;
        let status = response.status();
        if status.is_redirection() {
            let location = response
                .headers()
                .get(http::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| changed(step, "a redirect names no address"))?;
            url = same_origin_url(base, &url, location)
                .ok_or_else(|| changed(step, "a redirect leaves the site"))?;
            // The password is posted once: what follows a redirect is read.
            if !matches!(
                status,
                StatusCode::TEMPORARY_REDIRECT | StatusCode::PERMANENT_REDIRECT
            ) {
                method = Method::GET;
            } else if method == Method::POST {
                return Err(changed(step, "the site asks for the form again"));
            }
            continue;
        }
        if status == StatusCode::TOO_MANY_REQUESTS {
            return Err(refused(StatusCode::TOO_MANY_REQUESTS, "RATE_LIMITED"));
        }
        if status.is_server_error() {
            return Err(unavailable(step, &format!("answered {}", status.as_u16())));
        }
        let body = read_capped_text(response, MAX_RESPONSE_BYTES)
            .await
            .map_err(|_| unavailable(step, "the page could not be read"))?;
        // A refused password may be answered 401 or 422 with the form again.
        if !status.is_success() && !LOGIN_FORM.is_match(&body) {
            let why = format!("answered {} ({} bytes)", status.as_u16(), body.len());
            return Err(changed(step, &why));
        }
        return Ok(Page {
            path: url.path().to_owned(),
            body,
        });
    }
    Err(changed(step, "too many redirects"))
}

fn csrf_token(page: &str) -> Option<String> {
    INPUT
        .find_iter(page)
        .map(|input| input.as_str())
        .find(|input| input.contains(r#"name="_csrf_token""#))
        .and_then(|input| VALUE.captures(input))
        .map(|found| found[1].to_owned())
        .filter(|token| !token.is_empty())
}

/// Signs in on the site at `base_url` and reads the contracts of the home page.
pub async fn read_contracts(
    base_url: &str,
    identifier: &str,
    password: &str,
) -> Result<Vec<Contract>, NativeSidecarError> {
    let identifier = identifier.trim();
    if identifier.is_empty() || identifier.len() > 64 || password.is_empty() || password.len() > 200
    {
        return Err(refused(StatusCode::BAD_REQUEST, "INVALID_DATA"));
    }
    let base = Url::parse(&normalize_base_url(base_url))
        .map_err(|_| unavailable("login page", "the address is not a URL"))?;
    let client = build_http_client(
        MemoryCookieJar::new(),
        USER_AGENT,
        REQUEST_TIMEOUT,
        &[("Accept-Language", "fr-FR,fr;q=0.9")],
    )?;

    let login = send(&client, &base, "login page", None).await?;
    let token = csrf_token(&login.body).ok_or_else(|| {
        let why = format!("no form token ({} bytes)", login.body.len());
        changed("login page", &why)
    })?;
    let form = [
        ("_csrf_token", token.as_str()),
        ("username", identifier),
        ("password", password),
        ("submit", ""),
    ];
    let landed = send(&client, &base, "sign-in", Some(&form)).await?;

    if landed.path.contains("/mfa") {
        return Err(refused(StatusCode::NOT_IMPLEMENTED, "MFA_TYPE_UNSUPPORTED"));
    }
    if HOME.is_match(&landed.body) {
        return parse_contracts(&landed.body);
    }
    if CAPTCHA.is_match(&landed.body) {
        return Err(refused(StatusCode::FORBIDDEN, "CAPTCHA_BLOCKED"));
    }
    if LOGIN_FORM.is_match(&landed.body) {
        return Err(refused(StatusCode::UNAUTHORIZED, "INVALID_CREDENTIALS"));
    }
    let why = format!("an unknown page ({} bytes)", landed.body.len());
    Err(changed("sign-in", &why))
}

fn text(fragment: &str) -> String {
    unescape_then_strip_html_text(fragment).trim().to_owned()
}

fn first(pattern: &Regex, html: &str) -> Option<String> {
    pattern
        .captures(html)
        .map(|found| text(&found[1]))
        .filter(|value| !value.is_empty())
}

/// An amount as the site writes it: `1 234,50 €`, with ordinary or non-breaking spaces.
fn amount(written: &str) -> Option<Decimal> {
    let digits: String = text(written)
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '€')
        .map(|c| if c == ',' { '.' } else { c })
        .collect();
    Decimal::from_str_exact(&digits).ok()
}

/// The contracts of the home page, which must add up to the total it announces: a page read in
/// part is refused whole, so that no contract is ever lost or counted at zero.
pub fn parse_contracts(html: &str) -> Result<Vec<Contract>, NativeSidecarError> {
    let broken = |what: &str| changed("home page", what);
    let mut seen = HashSet::new();
    let mut contracts = Vec::new();
    for article in CONTRACT.captures_iter(html) {
        let article = &article[1];
        let name = NAME
            .captures(article)
            .ok_or_else(|| broken("a contract has no link to its page"))?;
        let id = name[1].to_owned();
        if !seen.insert(id.clone()) {
            return Err(broken("a contract is listed twice"));
        }
        let name = Some(text(&name[2]))
            .filter(|name| !name.is_empty())
            .ok_or_else(|| broken("a contract has no name"))?;
        let value_eur = PRICE
            .captures(article)
            .and_then(|found| amount(&found[1]))
            .ok_or_else(|| broken("a contract has no readable value"))?;
        let valued_on = first(&VALUED_ON, article)
            .and_then(|date| NaiveDate::parse_from_str(&date, "%d/%m/%Y").ok())
            .ok_or_else(|| broken("a contract has no readable date"))?;
        contracts.push(Contract {
            id,
            number: first(&NUMBER, article),
            name,
            kind: first(&KIND, article).ok_or_else(|| broken("a contract has no type"))?,
            insurer: first(&INSURER, article),
            value_eur,
            valued_on,
        });
    }
    if contracts.is_empty() {
        return Err(broken("no contract is listed"));
    }
    let announced = TOTAL
        .captures(html)
        .and_then(|found| amount(&found[1]))
        .ok_or_else(|| broken("no readable total"))?;
    if contracts.iter().map(|c| c.value_eur).sum::<Decimal>() != announced {
        tracing::warn!(
            "Lucya contracts do not add up to the announced total ({} listed)",
            contracts.len()
        );
        return Err(refused(StatusCode::BAD_GATEWAY, "PORTFOLIO_INCOMPLETE"));
    }
    Ok(contracts)
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    const LOGIN: &str = r#"<form id="login" method="post" class="form">
        <input type="hidden" name="_csrf_token" value="token.example-1"/>
        <input type="text" id="username" name="username" value=""/></form>"#;

    /// Invented contracts, laid out with the home page's tags and classes.
    fn contract(id: &str, kind: &str, name: &str, value: &str) -> String {
        format!(
            r#"<article class="contract">
              <header class="contract__header">
                <span class="label label--monochrome-secondary">{kind}</span>
                <span class="label label--monochrome">Date d'effet : <strong>01/02/2020</strong></span>
                <span class="label label--primary"># <strong>X{id}</strong></span>
                <img src="/logo.png" alt="Assureur Exemple">
              </header>
              <h3 class="contract__name">
                <a href="https://app.lucya.com/asv/espace-client/contrat/{id}">{name}</a>
              </h3>
              <span class="contract__price">{value}</span>
              <time class="contract__date" datetime="03/04/2030">à jour le 03/04/2030</time>
              <footer class="contract__footer"></footer>
            </article>"#
        )
    }

    fn home(total: &str, contracts: &[String]) -> String {
        format!(
            r#"<body class="layout-synthesis" data-controller="scroll">
            <article class="card card--important column--mid">
              <p>Au <strong>03/04/2030</strong>, l'encours</p>
              <h2 class="card__title">{total}</h2>
            </article>
            <section class="contracts">{}</section>
            <aside class="modal"><article class="modal__content">Souscrire</article></aside>
            </body>"#,
            contracts.concat()
        )
    }

    fn two_contracts(total: &str) -> String {
        home(
            total,
            &[
                contract(
                    "101",
                    "Assurance vie",
                    "Contrat Exemple",
                    "1&nbsp;234,50&nbsp;€",
                ),
                contract("102", "PER", "Retraite Exemple", "100,00\u{202f}€"),
            ],
        )
    }

    /// A fake site whose sign-in answers `after`.
    async fn site(after: ResponseTemplate) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(LOGIN_PATH))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("set-cookie", "PHPSESSID=session-example; Path=/")
                    .set_body_string(LOGIN),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(LOGIN_PATH))
            .and(header("cookie", "PHPSESSID=session-example"))
            .and(body_string_contains("_csrf_token=token.example-1"))
            .and(body_string_contains("username=ab12"))
            .and(body_string_contains("password=pw"))
            .respond_with(after)
            .expect(1)
            .mount(&server)
            .await;
        server
    }

    async fn land_on(landing: &str, page: &str) -> (MockServer, Result<Vec<Contract>, String>) {
        let server = site(ResponseTemplate::new(302).insert_header("location", landing)).await;
        Mock::given(method("GET"))
            .and(path(landing))
            .respond_with(ResponseTemplate::new(200).set_body_string(page))
            .with_priority(1)
            .mount(&server)
            .await;
        let read = read_contracts(&server.uri(), " ab12 ", "pw")
            .await
            .map_err(|refused| refused.detail);
        (server, read)
    }

    #[tokio::test]
    async fn a_sign_in_reads_every_contract_of_the_home_page() {
        let (server, read) = land_on("/asv/espace-client/", &two_contracts("1 334,50 €")).await;

        let contracts = read.expect("contracts");

        assert_eq!(
            contracts[0],
            Contract {
                id: "101".into(),
                number: Some("X101".into()),
                name: "Contrat Exemple".into(),
                kind: "Assurance vie".into(),
                insurer: Some("Assureur Exemple".into()),
                value_eur: Decimal::new(123_450, 2),
                valued_on: NaiveDate::from_ymd_opt(2030, 4, 3).unwrap(),
            }
        );
        assert_eq!(contracts[1].kind, "PER");
        assert_eq!(contracts[1].value_eur, Decimal::new(10_000, 2));
        let requests = server.received_requests().await.unwrap();
        assert!(requests.iter().all(|r| !r.url.path().contains("logout")));
    }

    #[tokio::test]
    async fn each_other_landing_has_its_own_code() {
        for (landing, page, code) in [
            (
                "/asv/espace-client/mfa/code",
                "<body></body>",
                "MFA_TYPE_UNSUPPORTED",
            ),
            (
                "/asv/espace-client/verification",
                r#"<div class="g-recaptcha"></div>"#,
                "CAPTCHA_BLOCKED",
            ),
            (
                "/asv/espace-client/erreur",
                "<body>?</body>",
                "UPSTREAM_FORMAT_CHANGED",
            ),
        ] {
            let (_server, read) = land_on(landing, page).await;

            assert_eq!(read.expect_err(landing), code);
        }
    }

    #[tokio::test]
    async fn the_form_shown_again_is_a_refused_password() {
        let server = site(ResponseTemplate::new(200).set_body_string(LOGIN)).await;

        let read = read_contracts(&server.uri(), "ab12", "pw").await;

        assert_eq!(read.expect_err("refused").detail, "INVALID_CREDENTIALS");
    }

    #[tokio::test]
    async fn the_password_never_follows_a_redirect_off_the_site() {
        let away =
            ResponseTemplate::new(307).insert_header("location", "https://elsewhere.example/");
        let server = site(away).await;

        let read = read_contracts(&server.uri(), "ab12", "pw").await;

        assert_eq!(read.expect_err("refused").detail, "UPSTREAM_FORMAT_CHANGED");
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }

    #[test]
    fn only_a_stub_on_this_machine_replaces_the_site() {
        assert_eq!(base_url("http://127.0.0.1:8123"), "http://127.0.0.1:8123");
        for other in [
            "",
            "https://elsewhere.example",
            "http://localhost.example",
            "http://user@127.0.0.1",
        ] {
            assert_eq!(base_url(other), DEFAULT_BASE_URL, "{other}");
        }
    }

    #[test]
    fn a_page_read_in_part_is_refused_whole() {
        let short = two_contracts("2 000,00 €");
        assert_eq!(
            parse_contracts(&short).unwrap_err().detail,
            "PORTFOLIO_INCOMPLETE"
        );
        for page in [
            home("0,00 €", &[]),
            home(
                "1,00 €",
                &[contract("1", "PER", "Sans valeur", "indisponible")],
            ),
            home(
                "2,00 €",
                &[
                    contract("1", "PER", "A", "1,00 €"),
                    contract("1", "PER", "B", "1,00 €"),
                ],
            ),
        ] {
            assert_eq!(
                parse_contracts(&page).unwrap_err().detail,
                "UPSTREAM_FORMAT_CHANGED"
            );
        }
    }
}
