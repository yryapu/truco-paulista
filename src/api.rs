//! Rotas HTTP e o WebSocket.
//!
//! Regra sem exceção neste módulo: **nenhuma rota aceita `jogador_id` do cliente.** Quem o
//! pedido é vem do extractor `Jogador`, que lê o cookie. É assim que o isolamento não depende
//! de cada handler se lembrar de conferir.

use crate::auth::{self, Jogador};
use crate::cartas::Carta;
use crate::economia;
use crate::mesa::{modo_do_nome, Estado};
use crate::regras::{Acao, Resposta};
use crate::webhooks;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::cookie::CookieJar;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tower_http::services::{ServeDir, ServeFile};

/// Erro de API em JSON. Mensagem em português e legível: o cliente mostra direto.
pub struct Falha(StatusCode, String);

impl IntoResponse for Falha {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "erro": self.1 }))).into_response()
    }
}

impl From<auth::ErroAuth> for Falha {
    fn from(e: auth::ErroAuth) -> Self {
        let codigo = match e {
            auth::ErroAuth::SemSessao => StatusCode::UNAUTHORIZED,
            auth::ErroAuth::Credenciais => StatusCode::UNAUTHORIZED,
            auth::ErroAuth::ApelidoEmUso => StatusCode::CONFLICT,
            _ => StatusCode::BAD_REQUEST,
        };
        Falha(codigo, e.to_string())
    }
}

impl From<anyhow::Error> for Falha {
    fn from(e: anyhow::Error) -> Self {
        // Erro conhecido volta com a mensagem dele; desconhecido vira 500 genérico, porque
        // texto de erro de banco não deve chegar ao navegador.
        if let Some(a) = e.downcast_ref::<auth::ErroAuth>() {
            return a.clone().into();
        }
        if let Some(c) = e.downcast_ref::<economia::ErroEconomia>() {
            return Falha(StatusCode::BAD_REQUEST, c.to_string());
        }
        tracing::error!("erro interno: {e:#}");
        Falha(
            StatusCode::INTERNAL_SERVER_ERROR,
            "algo deu errado no servidor".into(),
        )
    }
}

type R<T> = Result<T, Falha>;

pub fn rotas(estado: Estado) -> Router {
    Router::new()
        .route("/api/saude", get(saude))
        .route("/api/cadastrar", post(cadastrar))
        .route("/api/entrar", post(entrar))
        .route("/api/sair", post(sair))
        .route("/api/eu", get(eu))
        .route("/api/extrato", get(extrato))
        .route("/api/ranking", get(ranking))
        .route("/api/mesas", post(sentar))
        .route("/api/mesas/sair", post(levantar))
        .route("/api/webhooks", get(listar_webhooks).post(criar_webhook))
        .route("/api/webhooks/{id}", axum::routing::delete(apagar_webhook))
        .route("/ws", get(websocket))
        .fallback_service(ServeDir::new("web").fallback(ServeFile::new("web/index.html")))
        .with_state(estado)
}

/// Saúde do processo. Existe para o `healthcheck` do container ser um portão de verdade:
/// sem ele, o compose declara "subiu" quando o PID existe, não quando o serviço responde.
async fn saude(State(e): State<Estado>) -> R<Json<serde_json::Value>> {
    // Toca o banco: um processo vivo com banco inacessível não está saudável.
    sqlx::query("SELECT 1")
        .fetch_one(&e.db)
        .await
        .map_err(anyhow::Error::from)?;
    Ok(Json(
        json!({ "ok": true, "jogo": "truco-paulista", "versao": env!("CARGO_PKG_VERSION") }),
    ))
}

// ----- cadastro e sessão -----

#[derive(Deserialize)]
pub struct Credenciais {
    pub apelido: String,
    pub senha: String,
}

/// Cadastro: apelido, senha, e já entra. Dois campos, uma chamada, sem confirmação — é o que
/// "começar a jogar em menos de um minuto" exige.
async fn cadastrar(
    State(e): State<Estado>,
    jar: CookieJar,
    Json(c): Json<Credenciais>,
) -> R<(CookieJar, Json<serde_json::Value>)> {
    let id = auth::cadastrar(&e.db, c.apelido.trim(), &c.senha).await?;
    let token = auth::abrir_sessao(&e.db, id).await?;
    let perfil = economia::perfil(&e.db, id, c.apelido.trim()).await?;
    Ok((
        jar.add(auth::cookie_de_sessao(token)),
        Json(json!({ "ok": true, "perfil": perfil })),
    ))
}

async fn entrar(
    State(e): State<Estado>,
    jar: CookieJar,
    Json(c): Json<Credenciais>,
) -> R<(CookieJar, Json<serde_json::Value>)> {
    let id = auth::autenticar(&e.db, c.apelido.trim(), &c.senha).await?;
    let token = auth::abrir_sessao(&e.db, id).await?;
    let perfil = economia::perfil(&e.db, id, c.apelido.trim()).await?;
    Ok((
        jar.add(auth::cookie_de_sessao(token)),
        Json(json!({ "ok": true, "perfil": perfil })),
    ))
}

async fn sair(State(e): State<Estado>, jar: CookieJar) -> R<(CookieJar, Json<serde_json::Value>)> {
    if let Some(c) = jar.get(auth::COOKIE_SESSAO) {
        auth::fechar_sessao(&e.db, c.value()).await?;
    }
    Ok((
        jar.add(auth::cookie_de_saida()),
        Json(json!({ "ok": true })),
    ))
}

async fn eu(State(e): State<Estado>, j: Jogador) -> R<Json<economia::Perfil>> {
    Ok(Json(economia::perfil(&e.db, j.id, &j.apelido).await?))
}

/// Extrato do ledger. Só o próprio: `j.id` vem do cookie, não da query.
async fn extrato(State(e): State<Estado>, j: Jogador) -> R<Json<Vec<economia::Lancamento>>> {
    Ok(Json(economia::extrato(&e.db, j.id).await?))
}

async fn ranking(State(e): State<Estado>) -> R<Json<Vec<economia::LinhaDoRanking>>> {
    Ok(Json(economia::ranking(&e.db, 50).await?))
}

// ----- mesas -----

#[derive(Deserialize)]
pub struct PedidoDeMesa {
    pub modo: String,
    pub aposta: i64,
}

async fn sentar(
    State(e): State<Estado>,
    j: Jogador,
    Json(p): Json<PedidoDeMesa>,
) -> R<Json<serde_json::Value>> {
    let modo = modo_do_nome(&p.modo)
        .ok_or_else(|| Falha(StatusCode::BAD_REQUEST, "modo deve ser 1x1 ou 2x2".into()))?;
    let id = e.entrar(j.id, &j.apelido, modo, p.aposta).await?;
    Ok(Json(json!({ "mesa": id })))
}

async fn levantar(State(e): State<Estado>, j: Jogador) -> R<Json<serde_json::Value>> {
    e.sair(j.id).await?;
    Ok(Json(json!({ "ok": true })))
}

// ----- webhooks -----

#[derive(Deserialize)]
pub struct NovoWebhook {
    pub url: String,
}

#[derive(Serialize)]
pub struct WebhookCriado {
    pub id: i64,
    pub url: String,
    /// Mostrado **uma vez**, na criação. Depois não há como recuperá-lo.
    pub segredo: String,
    pub como_verificar: &'static str,
}

async fn criar_webhook(
    State(e): State<Estado>,
    j: Jogador,
    Json(n): Json<NovoWebhook>,
) -> R<Json<WebhookCriado>> {
    webhooks::validar_url(&n.url).map_err(|m| Falha(StatusCode::BAD_REQUEST, m.into()))?;
    let segredo = webhooks::novo_segredo();
    let r = sqlx::query(
        "INSERT INTO webhook (jogador_id, url, segredo, criado_em) VALUES (?, ?, ?, ?)",
    )
    .bind(j.id)
    .bind(&n.url)
    .bind(&segredo)
    .bind(crate::db::agora())
    .execute(&e.db)
    .await
    .map_err(anyhow::Error::from)?;
    Ok(Json(WebhookCriado {
        id: r.last_insert_rowid(),
        url: n.url,
        segredo,
        como_verificar: "HMAC-SHA256 do corpo exato com este segredo, comparado ao cabecalho X-Truco-Signature (formato sha256=<hex>)",
    }))
}

#[derive(Serialize)]
pub struct WebhookListado {
    pub id: i64,
    pub url: String,
    pub entregas: i64,
    pub ultima_falha: Option<String>,
}

async fn listar_webhooks(State(e): State<Estado>, j: Jogador) -> R<Json<Vec<WebhookListado>>> {
    // `WHERE jogador_id = ?` com o id do cookie: um jogador nunca lista o webhook de outro.
    let linhas: Vec<(i64, String, i64, Option<String>)> = sqlx::query_as(
        "SELECT w.id, w.url,
                (SELECT COUNT(*) FROM entrega d WHERE d.webhook_id = w.id),
                (SELECT d.erro FROM entrega d WHERE d.webhook_id = w.id AND d.erro IS NOT NULL
                 ORDER BY d.id DESC LIMIT 1)
         FROM webhook w WHERE w.jogador_id = ? ORDER BY w.id",
    )
    .bind(j.id)
    .fetch_all(&e.db)
    .await
    .map_err(anyhow::Error::from)?;
    Ok(Json(
        linhas
            .into_iter()
            .map(|(id, url, entregas, ultima_falha)| WebhookListado {
                id,
                url,
                entregas,
                ultima_falha,
            })
            .collect(),
    ))
}

async fn apagar_webhook(
    State(e): State<Estado>,
    j: Jogador,
    Path(id): Path<i64>,
) -> R<Json<serde_json::Value>> {
    // O `AND jogador_id = ?` é o que impede apagar webhook alheio sabendo o id dele.
    let r = sqlx::query("DELETE FROM webhook WHERE id = ? AND jogador_id = ?")
        .bind(id)
        .bind(j.id)
        .execute(&e.db)
        .await
        .map_err(anyhow::Error::from)?;
    if r.rows_affected() == 0 {
        return Err(Falha(
            StatusCode::NOT_FOUND,
            "webhook nao encontrado".into(),
        ));
    }
    Ok(Json(json!({ "ok": true })))
}

// ----- WebSocket -----

#[derive(Deserialize)]
pub struct Conexao {
    pub mesa: i64,
}

/// A mensagem que o cliente manda. **Não há campo de assento**: quem joga é quem a sessão
/// diz que é. Sem isso, qualquer cliente jogaria pela cadeira do adversário.
#[derive(Deserialize)]
#[serde(tag = "acao", rename_all = "snake_case")]
pub enum Comando {
    /// `{"acao":"jogar","carta":"🂡"}` — a carta é o caractere Unicode.
    Jogar {
        carta: Carta,
        #[serde(default)]
        encoberta: bool,
    },
    Pedir,
    Responder {
        resposta: Resposta,
    },
    Onze {
        aceita: bool,
    },
}

async fn websocket(
    ws: WebSocketUpgrade,
    State(e): State<Estado>,
    j: Jogador,
    Query(c): Query<Conexao>,
) -> Response {
    // O extractor `Jogador` já rodou: um WebSocket não autenticado nunca é aceito. O cookie
    // sobe no handshake por ser mesma origem — ver D-10.
    ws.on_upgrade(move |socket| conversa(socket, e, j, c.mesa))
}

async fn conversa(mut socket: WebSocket, estado: Estado, jogador: Jogador, mesa_id: i64) {
    let Some(mut rx) = estado.receptor(mesa_id).await else {
        let _ = socket
            .send(Message::Text(
                json!({"tipo":"erro","mensagem":"mesa nao encontrada"})
                    .to_string()
                    .into(),
            ))
            .await;
        return;
    };

    // Estado inicial: quem acabou de conectar precisa ver a mesa, não esperar o próximo lance.
    if !enviar_visao(&mut socket, &estado, mesa_id, jogador.id).await {
        return;
    }

    loop {
        tokio::select! {
            // Alguém mexeu na mesa: cada socket recalcula a **sua** visão. É isto que faz
            // um broadcast só servir quatro jogadores que veem coisas diferentes.
            v = rx.recv() => {
                match v {
                    Ok(_) => {
                        if !enviar_visao(&mut socket, &estado, mesa_id, jogador.id).await {
                            return;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => return,
                }
            }
            msg = socket.recv() => {
                let Some(Ok(msg)) = msg else { return };
                let Message::Text(txt) = msg else { continue };
                let comando: Comando = match serde_json::from_str(&txt) {
                    Ok(c) => c,
                    Err(err) => {
                        let _ = socket.send(Message::Text(
                            json!({"tipo":"erro","mensagem": format!("comando invalido: {err}")})
                                .to_string().into(),
                        )).await;
                        continue;
                    }
                };
                let acao = match comando {
                    Comando::Jogar { carta, encoberta } => Acao::Jogar { carta, encoberta },
                    Comando::Pedir => Acao::Pedir,
                    Comando::Responder { resposta } => Acao::Responder(resposta),
                    Comando::Onze { aceita } => Acao::DecidirOnze { aceita },
                };
                if let Err(err) = estado.aplicar(mesa_id, jogador.id, acao).await {
                    // Ação recusada volta para quem tentou, e só para ele: a mesa inteira não
                    // precisa saber que alguém clicou fora da vez.
                    let _ = socket.send(Message::Text(
                        json!({"tipo":"erro","mensagem": err.to_string()}).to_string().into(),
                    )).await;
                }
            }
        }
    }
}

async fn enviar_visao(
    socket: &mut WebSocket,
    estado: &Estado,
    mesa_id: i64,
    jogador_id: i64,
) -> bool {
    let Some(v) = estado.visao(mesa_id, jogador_id).await else {
        return false;
    };
    match serde_json::to_string(&v) {
        Ok(s) => socket.send(Message::Text(s.into())).await.is_ok(),
        Err(e) => {
            tracing::error!("visao nao serializou: {e}");
            false
        }
    }
}

use tokio::sync::broadcast;
