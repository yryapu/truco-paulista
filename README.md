# Truco Paulista

Truco paulista jogável no navegador, 1x1 e 2x2, servidor **autoritativo** em Rust, tempo real
por WebSocket. A carta trafega como o **próprio caractere Unicode**: `🂡 🂱 🃁 🃑`.

As regras não foram inventadas nem lembradas: cada uma tem fonte citada, e a origem de cada
fonte está no repositório de pesquisa —
**[poliorketikos-truco-paulista](https://github.com/yryapu/poliorketikos-truco-paulista)**.
Os comentários deste código citam os IDs `R-nn` (regra com fonte) e `D-nn` (decisão minha
onde as fontes silenciam ou se contradizem).

## Rodar

```bash
make subir      # docker compose up -d --build  ->  http://localhost:8411
make testar     # fmt + clippy -D warnings + 47 testes (unidade e integracao)
make e2e        # 7 testes de front num navegador real, em container
make derrubar
```

Sem Docker: `cargo run` e abra `http://localhost:8080`.

## O que a v1 tem

| Requisito | Onde está | Prova |
|---|---|---|
| Cadastro em menos de um minuto | apelido + senha, uma chamada, sem e-mail nem confirmação (`src/auth.rs`) | `e2e/truco.spec.js` mede o tempo e falha acima de 60 s |
| Sessão segura e isolada | token opaco de 256 bits em cookie `HttpOnly; SameSite=Strict`; **nenhuma rota aceita `jogador_id` do cliente** | `sem_sessao_nao_se_joga_e_logout_revoga_de_verdade` |
| Saldo de 1000 moedas, aposta livre | ledger append-only; saldo é `SUM(delta)` (`src/economia.rs`) | `partida_1x1_inteira_pelo_websocket_com_saldo_fechando` |
| Ranking e emblemas | dois eixos — vitórias e histórico (`src/economia.rs`) | `os_dois_eixos_de_emblema_medem_coisas_diferentes` |
| Webhooks | `partida.comecou` e `partida.terminou`, assinados com HMAC-SHA256 | `webhook_de_partida_chega_assinado_e_com_resultado` |
| Tempo real | WebSocket com visão por jogador (`src/mesa.rs`) | `mesa_2x2_forma_duplas_alternadas_e_isola_as_quatro_maos` |

## As três decisões que explicam a forma do código

### 1. O servidor é autoritativo sobre as cartas, e isso decide o resto

Há saldo apostado, então trapaça é o modo de falha que importa. Toda a regra vive em
`src/regras.rs` — puro, sem I/O, sem async. O cliente **não sabe o que é manilha**, não compara
força e não decide de quem é a vez: desenha o estado que chega e manda quatro intenções.

Consequência: `mesa::visao` é a **única** porta por onde o estado do jogo sai. A visão de um
assento leva as cartas dele e, dos outros, só a contagem. Carta encoberta sai como `null`.
Não existe caminho alternativo.

### 2. Nada de WASM no cliente — e a razão é de arquitetura, não de desempenho

O atrativo de Rust no navegador seria compartilhar o crate de regras. Aqui isso é
**contraindicado**: mandar o avaliador de força de carta para o cliente convida a duplicar a
autoridade, e duas cópias da regra divergem. A fronteira cliente/servidor deste jogo é a
fronteira de confiança. Soma-se o dado medido (o cliente é 100% DOM, 0% CPU — o pior caso para
WASM) e o custo de ferramenta. Argumento completo e condição de queda em
[`decisoes/D-11-wasm.md`](https://github.com/yryapu/poliorketikos-truco-paulista/blob/main/decisoes/D-11-wasm.md).

### 3. Playwright, porque truco precisa de 2 a 4 jogadores simultâneos

`browser.newContext()` dá N contextos com cookie jar isolado — a primitiva exata do requisito
"dados de cada jogador isolados dos outros". Isso me deixa **provar** o isolamento: o teste
compara o DOM de um jogador contra as cartas do outro. Razões e alternativas descartadas em
[`decisoes/D-12-teste-de-front.md`](https://github.com/yryapu/poliorketikos-truco-paulista/blob/main/decisoes/D-12-teste-de-front.md).

## Protocolo do WebSocket

Conecta em `/ws?mesa=<id>`. O cookie de sessão sobe sozinho no handshake (mesma origem), então
**não há token na URL** nem segundo mecanismo de autenticação.

Cliente -> servidor. Note que **não existe campo de assento**: quem joga é quem a sessão diz que
é, e sem isso um cliente modificado jogaria pela cadeira do adversário.

```json
{"acao":"jogar","carta":"🂡","encoberta":false}
{"acao":"pedir"}
{"acao":"responder","resposta":"aceito"}
{"acao":"onze","aceita":true}
```

`resposta` é `aceito`, `correr` ou `aumentar`.

Servidor -> cliente: um `{"tipo":"estado", …}` por mudança, já filtrado para o destinatário, e
`{"tipo":"erro","mensagem":"nao e a sua vez"}` só para quem errou.

## Webhooks

`POST /api/webhooks {"url":"https://…"}` devolve um segredo **uma única vez**. Cada entrega vai
com `X-Truco-Event` e `X-Truco-Signature: sha256=<hex>`, o HMAC-SHA256 do corpo exato.

Verificando em Python:

```python
import hmac, hashlib
esperado = "sha256=" + hmac.new(segredo.encode(), corpo_bruto, hashlib.sha256).hexdigest()
assert hmac.compare_digest(esperado, cabecalho_recebido)
```

O corpo de `partida.terminou` traz `resultado` com equipe vencedora, apelidos, pontos, mãos
jogadas e prêmio.

## Mapa do código

| Arquivo | O que decide |
|---|---|
| `src/cartas.rs` | as 40 cartas, o caractere Unicode, e a força dada a vira |
| `src/regras.rs` | o motor: rodadas, empates (R-08), escada do truco (R-06), mão de onze e de ferro |
| `src/mesa.rs` | estado vivo, pareamento, e a visão por jogador |
| `src/auth.rs` | cadastro, argon2id, sessão, e o extractor que sustenta o isolamento |
| `src/economia.rs` | ledger, aposta, prêmio, ranking, emblemas |
| `src/webhooks.rs` | assinatura, entrega, e o bloqueio de link-local |
| `src/api.rs` | rotas HTTP e o WebSocket |
| `web/` | cliente, um HTML + um CSS + um JS, sem passo de build |

## Riscos conhecidos

Declarados em vez de disfarçados. O detalhe está em `resultado.json`.

- **Abandono de partida em curso não tem tratamento.** Se um jogador fecha o navegador, a mesa
  fica esperando a vez dele para sempre. Falta relógio de turno e W.O. — é a lacuna mais
  visível da v1.
- **Webhook sem retentativa.** `tokio::spawn` com timeout de 5 s e uma tentativa. Fila durável
  é o conserto certo e não é v1; prefiro não ter retentativa a ter uma que finge garantia.
- **SSRF parcialmente mitigado.** Bloqueio link-local (`169.254/16`, `fe80::/10`), que é o alvo
  de metadados de nuvem. **Não** bloqueio loopback nem rede privada, de propósito: inutilizaria
  integrador na mesma máquina. A defesa completa precisa resolver o nome e conferir o IP no
  momento do envio, contra DNS rebinding.
- **Sem limite de taxa.** Nada impede milhares de cadastros ou de tentativas de senha.
- **Partida não sobrevive a reinício do processo.** O estado da mesa é memória; o banco guarda
  jogador, saldo e resultado, nunca carta.
- **Divirjo de uma fonte, conscientemente:** F-02 diz que pedir aumento em mão de onze é
  derrota imediata; eu recuso a ação. Razão em `D-06`.

## Licença

MIT.
