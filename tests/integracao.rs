//! Integração de ponta a ponta: HTTP + WebSocket + banco + webhook, no processo.
//!
//! O que este arquivo existe para provar, e que teste de unidade não prova:
//! 1. a carta trafega como **caractere Unicode** no JSON do WebSocket, de ponta a ponta;
//! 2. um jogador **não** recebe a mão do outro — o isolamento, medido no frame;
//! 3. uma partida inteira termina e o **saldo fecha** (quem ganhou +aposta, quem perdeu −aposta);
//! 4. o webhook chega com assinatura HMAC **verificável**;
//! 5. o ranking e os emblemas refletem o resultado.

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use truco::cartas::{baralho, Carta};

struct App {
    base: String,
    cliente: reqwest::Client,
}

async fn subir() -> App {
    // Banco em arquivo temporário único por teste: dois testes em paralelo não se misturam.
    let arq = std::env::temp_dir().join(format!("truco-teste-{}.db", rand::random::<u64>()));
    let url = format!("sqlite://{}", arq.display());
    let pool = truco::db::abrir(&url).await.expect("banco");
    let estado = truco::mesa::Estado::novo(pool);
    let app = truco::api::rotas(estado);
    let l = TcpListener::bind("127.0.0.1:0").await.expect("porta");
    // Porta 0: o sistema escolhe uma livre. Fixar porta em teste é a receita para colidir com
    // um container que já publicou a porta — foi exatamente o que me aconteceu à mão.
    let porta = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = axum::serve(l, app).await;
    });
    App {
        base: format!("http://127.0.0.1:{porta}"),
        cliente: reqwest::Client::new(),
    }
}

impl App {
    async fn cadastrar(&self, apelido: &str) -> String {
        let r = self
            .cliente
            .post(format!("{}/api/cadastrar", self.base))
            .json(&serde_json::json!({"apelido": apelido, "senha": "segredo123"}))
            .send()
            .await
            .expect("cadastro");
        assert!(
            r.status().is_success(),
            "cadastro de {apelido} falhou: {:?}",
            r.status()
        );
        let cookie = r
            .headers()
            .get(reqwest::header::SET_COOKIE)
            .expect("servidor deve mandar o cookie de sessao")
            .to_str()
            .unwrap()
            .to_string();
        // Confere as proteções do cookie aqui, onde elas aparecem no fio.
        assert!(cookie.contains("HttpOnly"), "cookie sem HttpOnly: {cookie}");
        assert!(
            cookie.contains("SameSite=Strict"),
            "cookie sem SameSite=Strict: {cookie}"
        );
        cookie.split(';').next().unwrap().to_string()
    }

    async fn get(&self, rota: &str, cookie: &str) -> Value {
        self.cliente
            .get(format!("{}{rota}", self.base))
            .header(reqwest::header::COOKIE, cookie)
            .send()
            .await
            .expect("get")
            .json()
            .await
            .expect("json")
    }

    async fn post(&self, rota: &str, cookie: &str, corpo: Value) -> (u16, Value) {
        let r = self
            .cliente
            .post(format!("{}{rota}", self.base))
            .header(reqwest::header::COOKIE, cookie)
            .json(&corpo)
            .send()
            .await
            .expect("post");
        let s = r.status().as_u16();
        (s, r.json().await.unwrap_or(Value::Null))
    }

    async fn sentar(&self, cookie: &str, modo: &str, aposta: i64) -> i64 {
        let (s, v) = self
            .post(
                "/api/mesas",
                cookie,
                serde_json::json!({"modo": modo, "aposta": aposta}),
            )
            .await;
        assert_eq!(s, 200, "sentar falhou: {v}");
        v["mesa"].as_i64().expect("id da mesa")
    }

    async fn ws(&self, cookie: &str, mesa: i64) -> Socket {
        let url = format!("{}/ws?mesa={mesa}", self.base.replace("http://", "ws://"));
        let mut req = url.into_client_request().unwrap();
        // O cookie de sessão no handshake: é assim que o WS se autentica, sem token na URL.
        req.headers_mut()
            .insert(reqwest::header::COOKIE.as_str(), cookie.parse().unwrap());
        let (stream, _) = tokio_tungstenite::connect_async(req)
            .await
            .expect("upgrade do ws");
        Socket {
            stream,
            ultimo: None,
        }
    }
}

struct Socket {
    stream: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    /// O último estado conhecido. **Isto é o conserto de um impasse que me custou dois
    /// diagnósticos errados**, e vale explicar porque é um erro fácil de repetir:
    ///
    /// um WebSocket é um fluxo de eventos, mas o jogo é uma máquina de estados. Minha primeira
    /// versão só sabia "esperar o próximo frame". Depois da fase de asserções, as duas filas
    /// ficavam vazias — e o jogo estava esperando exatamente que a `ana` jogasse. A `ana`
    /// bloqueava esperando um frame que ninguém tinha motivo para mandar: ela era a causa do
    /// silêncio. Impasse de 20 s, e o sintoma ("ws calou") apontava para a rede, não para mim.
    ///
    /// Um cliente real nunca tem esse bug porque guarda o estado renderizado e age sobre ele.
    /// O teste tem de guardar também.
    ultimo: Option<Value>,
}

impl Socket {
    /// Bloqueia até chegar um frame de estado, e devolve o **mais recente** da fila.
    ///
    /// O dreno é correção, não otimização: agir sobre um frame velho faz o bot ver "não é
    /// minha vez" num instante em que já era.
    async fn proximo_estado(&mut self) -> Value {
        let prazo = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            let resto = prazo.saturating_duration_since(tokio::time::Instant::now());
            assert!(!resto.is_zero(), "prazo esgotado esperando estado do ws");
            let msg = tokio::time::timeout(resto, self.stream.next())
                .await
                .expect("ws calou antes do prazo")
                .expect("ws fechou")
                .expect("frame invalido");
            let Message::Text(t) = msg else { continue };
            let v: Value = serde_json::from_str(&t).expect("frame nao e json");
            if v["tipo"] == "estado" {
                return self.drenar(v).await;
            }
        }
    }

    /// Consome o que já está na fila e devolve o estado mais novo.
    async fn drenar(&mut self, mut ultimo: Value) -> Value {
        loop {
            let proximo =
                tokio::time::timeout(std::time::Duration::from_millis(40), self.stream.next())
                    .await;
            match proximo {
                Ok(Some(Ok(Message::Text(t)))) => {
                    if let Ok(v) = serde_json::from_str::<Value>(&t) {
                        if v["tipo"] == "estado" {
                            ultimo = v;
                        }
                    }
                }
                Ok(Some(Ok(_))) => continue,
                // fila vazia (timeout) ou socket encerrado: o que temos e o mais novo
                _ => return ultimo,
            }
        }
    }

    /// O estado corrente: o último conhecido, ou espera por um se ainda não há nenhum.
    async fn estado(&mut self) -> Value {
        if let Some(v) = &self.ultimo {
            return v.clone();
        }
        let v = self.proximo_estado().await;
        self.ultimo = Some(v.clone());
        v
    }

    /// O estado corrente, se ele satisfizer `cond`; senão espera frames novos até satisfazer.
    async fn estado_ate(&mut self, cond: impl Fn(&Value) -> bool) -> Value {
        loop {
            let v = self.estado().await;
            if cond(&v) {
                return v;
            }
            self.invalidar(); // forca esperar um frame novo em vez de girar sobre o cache
        }
    }

    fn invalidar(&mut self) {
        self.ultimo = None;
    }

    async fn enviar(&mut self, v: Value) {
        self.stream
            .send(Message::Text(v.to_string().into()))
            .await
            .expect("envio no ws");
        // A minha ação muda o estado: o cache envelheceu no instante do envio.
        self.invalidar();
    }

    async fn erro_ate(&mut self) -> String {
        let prazo = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let resto = prazo.saturating_duration_since(tokio::time::Instant::now());
            assert!(!resto.is_zero(), "prazo esgotado esperando erro do ws");
            let msg = tokio::time::timeout(resto, self.stream.next())
                .await
                .expect("ws calou")
                .expect("ws fechou")
                .expect("frame invalido");
            let Message::Text(t) = msg else { continue };
            let v: Value = serde_json::from_str(&t).unwrap();
            if v["tipo"] == "erro" {
                return v["mensagem"].as_str().unwrap().to_string();
            }
            if v["tipo"] == "estado" {
                self.ultimo = Some(v);
            }
        }
    }
}

fn e_carta_do_truco(s: &str) -> bool {
    let validas: HashSet<String> = baralho()
        .iter()
        .map(|c: &Carta| c.unicode().to_string())
        .collect();
    validas.contains(s)
}

/// O teste central: partida 1x1 inteira, do cadastro ao prêmio pago.
#[tokio::test]
async fn partida_1x1_inteira_pelo_websocket_com_saldo_fechando() {
    std::env::set_var("PAUSA_MAO_MS", "10");
    let app = subir().await;
    let ana = app.cadastrar("ana").await;
    let bia = app.cadastrar("bia").await;
    const APOSTA: i64 = 100;

    assert_eq!(
        app.get("/api/eu", &ana).await["saldo"],
        1000,
        "todo jogador comeca com 1000"
    );

    let m1 = app.sentar(&ana, "1x1", APOSTA).await;
    let m2 = app.sentar(&bia, "1x1", APOSTA).await;
    assert_eq!(m1, m2, "o pareamento tem de juntar as duas na MESMA mesa");

    // A aposta sai do saldo quando a mesa enche.
    assert_eq!(app.get("/api/eu", &ana).await["saldo"], 1000 - APOSTA);
    assert_eq!(app.get("/api/eu", &bia).await["saldo"], 1000 - APOSTA);

    let mut sa = app.ws(&ana, m1).await;
    let mut sb = app.ws(&bia, m1).await;

    let ea = sa
        .estado_ate(|v| !v["aguardando"].as_bool().unwrap_or(true))
        .await;
    let eb = sb
        .estado_ate(|v| !v["aguardando"].as_bool().unwrap_or(true))
        .await;

    // ---- (1) a carta é o caractere Unicode, no fio
    let mao_a: Vec<String> = ea["sua_mao"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap().to_string())
        .collect();
    assert_eq!(mao_a.len(), 3, "R-05: tres cartas");
    for c in &mao_a {
        assert_eq!(
            c.chars().count(),
            1,
            "a carta tem de ser UM caractere, veio {c:?}"
        );
        assert!(e_carta_do_truco(c), "{c:?} nao e carta do baralho de truco");
    }
    let vira = ea["vira"].as_str().unwrap();
    assert!(e_carta_do_truco(vira), "vira invalida: {vira:?}");
    assert!(
        ea["manilha"].is_string(),
        "a mesa precisa dizer qual valor e manilha"
    );

    // ---- (2) isolamento: o frame de ana não contém nenhuma carta de bia
    let mao_b: HashSet<String> = eb["sua_mao"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap().to_string())
        .collect();
    let frame_a = serde_json::to_string(&ea).unwrap();
    for c in &mao_b {
        assert!(
            !frame_a.contains(c.as_str()) || mao_a.contains(c) || vira == c.as_str(),
            "a carta {c} de bia apareceu no frame de ana"
        );
    }
    assert!(
        ea["maos_visiveis"]
            .as_object()
            .map(|o| o.is_empty())
            .unwrap_or(true),
        "em mao normal nenhuma mao alheia e visivel (R-10 e a unica excecao)"
    );
    // Dos outros, só a contagem.
    let outro = ea["jogadores"]
        .as_array()
        .unwrap()
        .iter()
        .find(|j| j["assento"] != ea["seu_assento"])
        .unwrap();
    assert_eq!(
        outro["cartas"], 3,
        "do adversario se sabe quantas, nunca quais"
    );
    assert!(outro.get("mao").is_none() && outro.get("sua_mao").is_none());

    // ---- (3) joga a partida inteira com um bot bobo em cada ponta
    let mut vencedora = None;
    for _ in 0..600 {
        if let Some(v) = jogar_um_lance(&mut sa, 0).await {
            vencedora = Some(v);
            break;
        }
        if let Some(v) = jogar_um_lance(&mut sb, 1).await {
            vencedora = Some(v);
            break;
        }
    }
    let vencedora = vencedora.expect("a partida tinha de terminar em 600 lances");

    // ---- saldo: o bolo fecha, nem cria nem destroi moeda
    // Pequena espera: a liquidação roda depois do último lance.
    let mut saldos = (0, 0);
    for _ in 0..80 {
        let a = app.get("/api/eu", &ana).await["saldo"].as_i64().unwrap();
        let b = app.get("/api/eu", &bia).await["saldo"].as_i64().unwrap();
        saldos = (a, b);
        if a + b == 2000 && (a == 1000 + APOSTA || b == 1000 + APOSTA) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let (sa_final, sb_final) = saldos;
    assert_eq!(
        sa_final + sb_final,
        2000,
        "o total de moedas nao pode mudar: {sa_final} + {sb_final}"
    );
    let (ganhou, perdeu) = if vencedora == 0 {
        (sa_final, sb_final)
    } else {
        (sb_final, sa_final)
    };
    assert_eq!(ganhou, 1000 + APOSTA, "quem venceu fica com 1000 + aposta");
    assert_eq!(perdeu, 1000 - APOSTA, "quem perdeu fica com 1000 - aposta");

    // ---- (5) ranking e emblemas
    let rank: Value = app.get("/api/ranking", &ana).await;
    let linhas = rank.as_array().unwrap();
    let total_v: i64 = linhas.iter().map(|l| l["vitorias"].as_i64().unwrap()).sum();
    assert_eq!(
        total_v, 1,
        "uma partida encerrada gera exatamente uma vitoria no ranking"
    );
    let campeao = linhas.iter().find(|l| l["vitorias"] == 1).unwrap();
    assert_eq!(campeao["posicao"], 1, "quem ganhou lidera");
    assert_eq!(campeao["derrotas"], 0);
    assert_eq!(campeao["emblema_vitorias"], "Primeira Mão");
    assert_eq!(
        campeao["emblema_historico"], "Estreante",
        "1 partida nao faz veterano"
    );
}

/// Lê o estado de um socket e faz a coisa óbvia. Devolve a equipe vencedora quando a partida
/// acaba. O bot não decide bem: decide **legal**, que é o que o teste precisa.
async fn jogar_um_lance(s: &mut Socket, _assento: usize) -> Option<u8> {
    let v = s
        .estado_ate(|v| {
            v["vencedora"].is_u64()
                || matches!(
                    v["fase"]["fase"].as_str(),
                    Some("jogando" | "respondendo" | "decidir_onze")
                )
        })
        .await;
    if let Some(eq) = v["vencedora"].as_u64() {
        return Some(eq as u8);
    }
    let meu = v["seu_assento"].as_i64().unwrap();
    match v["fase"]["fase"].as_str() {
        Some("jogando") if v["fase"]["vez"].as_i64() == Some(meu) => {
            let carta = v["sua_mao"][0].as_str().unwrap().to_string();
            s.enviar(serde_json::json!({"acao":"jogar","carta":carta}))
                .await;
        }
        Some("respondendo") if v["fase"]["responde"].as_i64() == Some(meu) => {
            s.enviar(serde_json::json!({"acao":"responder","resposta":"aceito"}))
                .await;
        }
        Some("decidir_onze") if v["fase"]["decide"].as_i64() == Some(meu) => {
            s.enviar(serde_json::json!({"acao":"onze","aceita":true}))
                .await;
        }
        _ => {
            // Não é a minha vez. Invalida o cache para que a próxima chamada **espere** um
            // frame novo em vez de reexaminar o mesmo estado para sempre.
            s.invalidar();
        }
    }
    None
}

/// O WebSocket recusa ação fora da vez, e recusa carta que o jogador não tem — as duas
/// defesas que impedem um cliente modificado de jogar pelo adversário.
#[tokio::test]
async fn websocket_recusa_jogada_fora_da_vez_e_carta_alheia() {
    std::env::set_var("PAUSA_MAO_MS", "10");
    let app = subir().await;
    let ana = app.cadastrar("ana").await;
    let bia = app.cadastrar("bia").await;
    let m = app.sentar(&ana, "1x1", 0).await;
    app.sentar(&bia, "1x1", 0).await;
    let mut sa = app.ws(&ana, m).await;
    let mut sb = app.ws(&bia, m).await;

    let ea = sa.estado_ate(|v| v["fase"]["fase"] == "jogando").await;
    let eb = sb.estado_ate(|v| v["fase"]["fase"] == "jogando").await;
    let vez = ea["fase"]["vez"].as_i64().unwrap();
    let (fora, dentro) = if ea["seu_assento"].as_i64() == Some(vez) {
        (&mut sb, &mut sa)
    } else {
        (&mut sa, &mut sb)
    };
    let mao_de_quem_nao_e_a_vez = if ea["seu_assento"].as_i64() == Some(vez) {
        eb["sua_mao"][0].as_str().unwrap().to_string()
    } else {
        ea["sua_mao"][0].as_str().unwrap().to_string()
    };

    // Jogar fora da vez: o servidor responde erro, e só para quem tentou.
    fora.enviar(serde_json::json!({"acao":"jogar","carta":mao_de_quem_nao_e_a_vez}))
        .await;
    let e = fora.erro_ate().await;
    assert_eq!(e, "nao e a sua vez", "erro inesperado: {e}");

    // Quem tem a vez tentando jogar carta que não tem: recusado por não estar na mão.
    let minha = if ea["seu_assento"].as_i64() == Some(vez) {
        &ea
    } else {
        &eb
    };
    let nao_tenho = baralho()
        .into_iter()
        .map(|c| c.unicode().to_string())
        .find(|c| {
            !minha["sua_mao"]
                .as_array()
                .unwrap()
                .iter()
                .any(|x| x.as_str() == Some(c))
        })
        .unwrap();
    dentro
        .enviar(serde_json::json!({"acao":"jogar","carta":nao_tenho}))
        .await;
    let e = dentro.erro_ate().await;
    assert_eq!(e, "voce nao tem essa carta", "erro inesperado: {e}");

    // E comando mal formado não derruba a conexão.
    dentro
        .enviar(serde_json::json!({"acao":"jogar","carta":"AS"}))
        .await;
    let e = dentro.erro_ate().await;
    assert!(e.contains("comando invalido"), "erro inesperado: {e}");
}

/// Mesa 2x2: quatro jogadores, equipes alternando, e ninguém vê carta de ninguém.
#[tokio::test]
async fn mesa_2x2_forma_duplas_alternadas_e_isola_as_quatro_maos() {
    std::env::set_var("PAUSA_MAO_MS", "10");
    let app = subir().await;
    let mut cookies = Vec::new();
    for n in ["ana", "bia", "caio", "dora"] {
        cookies.push(app.cadastrar(n).await);
    }
    let mesa = app.sentar(&cookies[0], "2x2", 10).await;
    for c in &cookies[1..] {
        assert_eq!(
            app.sentar(c, "2x2", 10).await,
            mesa,
            "os quatro na mesma mesa"
        );
    }
    let mut socks = Vec::new();
    for c in &cookies {
        socks.push(app.ws(c, mesa).await);
    }
    let mut maos = Vec::new();
    let mut estados = Vec::new();
    for s in socks.iter_mut() {
        let e = s
            .estado_ate(|v| !v["aguardando"].as_bool().unwrap_or(true))
            .await;
        maos.push(
            e["sua_mao"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c.as_str().unwrap().to_string())
                .collect::<Vec<_>>(),
        );
        estados.push(e);
    }
    // As quatro mãos são disjuntas e nenhuma repete carta: 12 cartas distintas.
    let todas: HashSet<&String> = maos.iter().flatten().collect();
    assert_eq!(todas.len(), 12, "as quatro maos tem de ser disjuntas");

    // Equipes alternam: 0 e 2 contra 1 e 3.
    for (i, e) in estados.iter().enumerate() {
        assert_eq!(e["seu_assento"], i as i64);
        assert_eq!(
            e["sua_equipe"],
            (i % 2) as i64,
            "assento {i} na equipe errada"
        );
        let parceiro = e["jogadores"]
            .as_array()
            .unwrap()
            .iter()
            .find(|j| j["assento"].as_i64() == Some(((i + 2) % 4) as i64))
            .unwrap();
        assert_eq!(
            parceiro["equipe"],
            (i % 2) as i64,
            "o parceiro esta a duas cadeiras"
        );
        // Nem a do parceiro é visível em mão normal (D-07).
        assert!(e["maos_visiveis"]
            .as_object()
            .map(|o| o.is_empty())
            .unwrap_or(true));
    }
}

/// Webhook: chega, traz o resultado, e a assinatura HMAC confere com o segredo entregue.
#[tokio::test]
async fn webhook_de_partida_chega_assinado_e_com_resultado() {
    std::env::set_var("PAUSA_MAO_MS", "10");
    let app = subir().await;
    let ana = app.cadastrar("ana").await;
    let bia = app.cadastrar("bia").await;

    // Receptor de webhook: um servidor de teste que guarda corpo e cabeçalhos.
    type Caixa = Arc<Mutex<Vec<(String, String, String)>>>;
    let caixa: Caixa = Arc::new(Mutex::new(Vec::new()));
    let c2 = caixa.clone();
    let receptor = axum::Router::new().route(
        "/hook",
        axum::routing::post(move |cabecalhos: axum::http::HeaderMap, corpo: String| {
            let c = c2.clone();
            async move {
                let ev = cabecalhos
                    .get("x-truco-event")
                    .map(|v| v.to_str().unwrap().to_string())
                    .unwrap_or_default();
                let sig = cabecalhos
                    .get("x-truco-signature")
                    .map(|v| v.to_str().unwrap().to_string())
                    .unwrap_or_default();
                c.lock().unwrap().push((ev, sig, corpo));
                "ok"
            }
        }),
    );
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let porta_hook = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = axum::serve(l, receptor).await;
    });

    let (s, v) = app
        .post(
            "/api/webhooks",
            &ana,
            serde_json::json!({"url": format!("http://127.0.0.1:{porta_hook}/hook")}),
        )
        .await;
    assert_eq!(s, 200, "registro de webhook falhou: {v}");
    let segredo = v["segredo"]
        .as_str()
        .expect("o segredo e entregue uma vez")
        .to_string();
    assert!(segredo.starts_with("whsec_"));

    let m = app.sentar(&ana, "1x1", 0).await;
    app.sentar(&bia, "1x1", 0).await;
    let mut sa = app.ws(&ana, m).await;
    let mut sb = app.ws(&bia, m).await;
    for _ in 0..600 {
        if jogar_um_lance(&mut sa, 0).await.is_some() {
            break;
        }
        if jogar_um_lance(&mut sb, 1).await.is_some() {
            break;
        }
    }

    // Espera as duas entregas.
    let mut recebidas = Vec::new();
    for _ in 0..100 {
        recebidas = caixa.lock().unwrap().clone();
        if recebidas.len() >= 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(
        recebidas.len() >= 2,
        "esperava partida.comecou e partida.terminou, veio {recebidas:?}"
    );

    let eventos: Vec<&str> = recebidas.iter().map(|(e, _, _)| e.as_str()).collect();
    assert!(
        eventos.contains(&"partida.comecou"),
        "faltou partida.comecou: {eventos:?}"
    );
    assert!(
        eventos.contains(&"partida.terminou"),
        "faltou partida.terminou: {eventos:?}"
    );

    for (ev, sig, corpo) in &recebidas {
        // A assinatura é do corpo EXATO, com o segredo entregue no registro.
        assert_eq!(
            *sig,
            truco::webhooks::assinar(&segredo, corpo.as_bytes()),
            "assinatura nao confere para {ev}"
        );
        let j: Value = serde_json::from_str(corpo).unwrap();
        assert_eq!(j["evento"], ev.as_str());
        assert_eq!(j["modo"], "um_vs_um");
        assert_eq!(j["jogadores"].as_array().unwrap().len(), 2);
        if ev == "partida.terminou" {
            let r = &j["resultado"];
            assert!(
                r["equipe_vencedora"].is_u64(),
                "resultado sem equipe vencedora: {j}"
            );
            assert_eq!(r["vencedores"].as_array().unwrap().len(), 1);
            let pontos = r["pontos"].as_array().unwrap();
            let max = pontos.iter().map(|p| p.as_i64().unwrap()).max().unwrap();
            assert!(
                max >= 12,
                "R-09: a partida termina em 12 pontos, veio {pontos:?}"
            );
        } else {
            assert!(
                j["resultado"].is_null(),
                "partida.comecou nao tem resultado ainda"
            );
        }
    }

    // Um jogador não lista nem apaga webhook de outro.
    let lista_bia: Value = app.get("/api/webhooks", &bia).await;
    assert_eq!(
        lista_bia.as_array().unwrap().len(),
        0,
        "bia nao ve o webhook de ana"
    );
    let id = v["id"].as_i64().unwrap();
    let r = app
        .cliente
        .delete(format!("{}/api/webhooks/{id}", app.base))
        .header(reqwest::header::COOKIE, &bia)
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.status().as_u16(),
        404,
        "bia nao pode apagar webhook de ana"
    );
}

/// Aposta maior que o saldo é recusada, e não deixa a mesa num estado meio pago.
#[tokio::test]
async fn aposta_acima_do_saldo_e_recusada() {
    let app = subir().await;
    let ana = app.cadastrar("ana").await;
    let (s, v) = app
        .post(
            "/api/mesas",
            &ana,
            serde_json::json!({"modo":"1x1","aposta":5000}),
        )
        .await;
    assert_eq!(s, 400, "esperava recusa, veio {v}");
    assert_eq!(v["erro"], "saldo insuficiente para essa aposta");
    assert_eq!(
        app.get("/api/eu", &ana).await["saldo"],
        1000,
        "o saldo nao pode ter mexido"
    );
}

/// Sessão: sem cookie não se faz nada; depois do logout o cookie antigo morre.
#[tokio::test]
async fn sem_sessao_nao_se_joga_e_logout_revoga_de_verdade() {
    let app = subir().await;
    let ana = app.cadastrar("ana").await;
    for rota in ["/api/eu", "/api/extrato", "/api/webhooks"] {
        let r = app
            .cliente
            .get(format!("{}{rota}", app.base))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status().as_u16(), 401, "{rota} devia exigir sessao");
    }
    assert_eq!(app.get("/api/eu", &ana).await["apelido"], "ana");
    let r = app
        .cliente
        .post(format!("{}/api/sair", app.base))
        .header(reqwest::header::COOKIE, &ana)
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success());
    // O token velho não vale mais: revogação é DELETE na tabela, não espera de expiração.
    let r = app
        .cliente
        .get(format!("{}/api/eu", app.base))
        .header(reqwest::header::COOKIE, &ana)
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.status().as_u16(),
        401,
        "o cookie de antes do logout ainda funcionou"
    );
}

/// O extrato mostra o ledger do próprio jogador, começando pelo bônus inicial.
#[tokio::test]
async fn extrato_mostra_o_ledger_do_proprio_jogador() {
    let app = subir().await;
    let ana = app.cadastrar("ana").await;
    let bia = app.cadastrar("bia").await;
    let e: Value = app.get("/api/extrato", &ana).await;
    let l = e.as_array().unwrap();
    assert_eq!(l.len(), 1, "so o bonus inicial ainda");
    assert_eq!(l[0]["delta"], 1000);
    assert_eq!(l[0]["motivo"], "bonus_inicial");

    app.sentar(&ana, "1x1", 70).await;
    app.sentar(&bia, "1x1", 70).await;
    let e: Value = app.get("/api/extrato", &ana).await;
    let l = e.as_array().unwrap();
    assert_eq!(l[0]["motivo"], "aposta", "o mais recente vem primeiro");
    assert_eq!(l[0]["delta"], -70);
    let soma: i64 = l.iter().map(|x| x["delta"].as_i64().unwrap()).sum();
    assert_eq!(soma, 930, "o saldo e a soma do ledger");
}
