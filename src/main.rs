//! Ponto de entrada. Fino de propósito: tudo que é lógica mora na lib, para ser testável.

use truco::{api, db, mesa::Estado};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "truco=info,tower_http=warn".into()),
        )
        .init();

    let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite://truco.db".into());
    let porta: u16 = std::env::var("PORTA")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);

    let pool = db::abrir(&url).await?;

    // Antes de aceitar qualquer conexão: devolve as apostas de partidas que o reinício
    // anterior deixou pelo caminho. Sem isto, moeda debitada numa partida interrompida
    // ficava presa para sempre. Ver `economia::recuperar_partidas_orfas`.
    match truco::economia::recuperar_partidas_orfas(&pool).await {
        Ok(0) => {}
        Ok(n) => tracing::warn!("{n} partida(s) orfa(s) encerrada(s) com estorno das apostas"),
        Err(e) => tracing::error!("nao consegui recuperar partidas orfas: {e:#}"),
    }

    let estado = Estado::novo(pool);
    let app = api::rotas(estado).layer(tower_http::trace::TraceLayer::new_for_http());

    // 0.0.0.0 porque dentro de container é a única forma de o mapeamento de porta alcançar.
    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], porta));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("truco paulista servindo em http://localhost:{porta}");
    axum::serve(listener, app).await?;
    Ok(())
}
