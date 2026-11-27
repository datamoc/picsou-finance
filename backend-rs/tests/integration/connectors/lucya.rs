//! The Lucya refresh over HTTP, the site stubbed with invented pages: the accounts it writes, what
//! a later refresh changes, and that a refusal writes nothing.

use picsou::testing::{Client, Response, TestApp};
use rust_decimal::Decimal;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::support::{admin, app_with_sidecar, count, d};

const URL: &str = "LUCYA_BASE_URL";
const LOGIN_PATH: &str = "/asv/espace-client/connexion";
const HOME_PATH: &str = "/asv/espace-client/";
const LOGIN: &str = r#"<form id="login" method="post">
    <input type="hidden" name="_csrf_token" value="token.example-1"/></form>"#;

fn contract(id: &str, kind: &str, name: &str, euros: u32) -> (String, u32) {
    let article = format!(
        r#"<article class="contract">
          <span class="label label--monochrome-secondary">{kind}</span>
          <h3 class="contract__name"><a href="/asv/espace-client/contrat/{id}">{name}</a></h3>
          <span class="contract__price">{euros},00 €</span>
          <time class="contract__date" datetime="03/04/2030">à jour</time>
        </article>"#
    );
    (article, euros)
}

fn home(contracts: &[(String, u32)]) -> String {
    let total: u32 = contracts.iter().map(|c| c.1).sum();
    let articles: String = contracts.iter().map(|c| c.0.as_str()).collect();
    format!(
        r#"<body class="layout-synthesis">
        <article class="card card--important"><h2 class="card__title">{total},00 €</h2></article>
        {articles}</body>"#
    )
}

fn life(euros: u32) -> (String, u32) {
    contract("101", "Assurance vie", "Contrat Exemple", euros)
}

fn retirement(euros: u32) -> (String, u32) {
    contract("102", "PER", "Retraite Exemple", euros)
}

/// Makes the site's sign-in land on `page`.
async fn serve(site: &MockServer, page: String) {
    site.reset().await;
    Mock::given(method("GET"))
        .and(path(LOGIN_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(LOGIN))
        .mount(site)
        .await;
    Mock::given(method("POST"))
        .and(path(LOGIN_PATH))
        .respond_with(ResponseTemplate::new(302).insert_header("location", HOME_PATH))
        .mount(site)
        .await;
    Mock::given(method("GET"))
        .and(path(HOME_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(page))
        .mount(site)
        .await;
}

async fn setup(page: String) -> (MockServer, TestApp, Client, i64) {
    let site = MockServer::start().await;
    serve(&site, page).await;
    let app = app_with_sidecar(URL, &site).await;
    let (client, member_id) = admin(&app).await;
    (site, app, client, member_id)
}

async fn refresh(client: &Client) -> Response {
    client
        .post("/api/lucya/refresh")
        .json(&json!({ "identifier": "ab12", "password": "pw" }))
        .send()
        .await
}

/// The live Lucya accounts as (external id, type, balance).
async fn live(app: &TestApp, member_id: i64) -> Vec<(String, String, Decimal)> {
    picsou::db::query_as(
        "SELECT external_account_id, CAST(type AS TEXT), current_balance FROM account \
         WHERE member_id = $1 AND provider = 'Lucya' AND deleted_at IS NULL \
         ORDER BY external_account_id",
    )
    .bind(member_id)
    .fetch_all(&app.state.db)
    .await
    .expect("accounts")
}

fn row(id: &str, kind: &str, balance: &str) -> (String, String, Decimal) {
    (id.to_owned(), kind.to_owned(), d(balance))
}

#[tokio::test]
async fn a_refresh_writes_one_account_per_contract_and_keeps_no_session() {
    let (_site, app, client, member_id) = setup(home(&[life(1000), retirement(250)])).await;

    let response = refresh(&client).await;

    assert_eq!(response.status, 200, "{}", response.text());
    assert_eq!(response.json()["contractCount"], 2);
    assert_eq!(
        live(&app, member_id).await,
        vec![
            row("lucya_101", "ASSURANCE_VIE", "1000"),
            row("lucya_102", "PER", "250"),
        ]
    );
    assert_eq!(
        count(&app, "SELECT count(*) FROM balance_snapshot").await,
        2
    );
    assert_eq!(count(&app, "SELECT count(*) FROM sidecar_session").await, 0);
}

#[tokio::test]
async fn a_later_refresh_updates_restores_and_closes() {
    let (site, app, client, member_id) = setup(home(&[life(1000), retirement(250)])).await;
    assert_eq!(refresh(&client).await.status, 200);
    picsou::db::query("UPDATE account SET deleted_at = now() WHERE external_account_id = $1")
        .bind("lucya_101")
        .execute(&app.state.db)
        .await
        .unwrap();
    serve(&site, home(&[life(1100)])).await;

    let response = refresh(&client).await;

    assert_eq!(response.status, 200, "{}", response.text());
    assert_eq!(
        live(&app, member_id).await,
        vec![
            row("lucya_101", "ASSURANCE_VIE", "1100"),
            row("lucya_102", "PER", "0"),
        ]
    );
    assert_eq!(count(&app, "SELECT count(*) FROM account").await, 2);
}

#[tokio::test]
async fn a_deleted_contract_the_site_no_longer_lists_stays_deleted() {
    let (site, app, client, member_id) = setup(home(&[life(1000), retirement(250)])).await;
    assert_eq!(refresh(&client).await.status, 200);
    picsou::db::query("UPDATE account SET deleted_at = now() WHERE external_account_id = $1")
        .bind("lucya_102")
        .execute(&app.state.db)
        .await
        .unwrap();
    serve(&site, home(&[life(1000)])).await;

    assert_eq!(refresh(&client).await.status, 200);

    assert_eq!(
        live(&app, member_id).await,
        vec![row("lucya_101", "ASSURANCE_VIE", "1000")]
    );
}

#[tokio::test]
async fn a_refusal_writes_nothing() {
    let partial = home(&[life(1000)]).replace(">1000,00 €</h2>", ">9999,00 €</h2>");
    let unknown = home(&[life(1000), contract("103", "Autre placement", "Inconnu", 1)]);
    for (page, code) in [
        (LOGIN.to_owned(), "INVALID_CREDENTIALS"),
        (partial, "PORTFOLIO_INCOMPLETE"),
        (unknown, "UPSTREAM_FORMAT_CHANGED"),
    ] {
        let (_site, app, client, _) = setup(page).await;

        let response = refresh(&client).await;

        assert_eq!(response.status, 422, "{code}: {}", response.text());
        assert_eq!(response.json()["code"], code);
        assert_eq!(count(&app, "SELECT count(*) FROM account").await, 0);
    }
}
