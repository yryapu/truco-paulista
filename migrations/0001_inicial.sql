-- Esquema inicial. Decisões que o esquema carrega:
--
-- 1. O saldo NÃO é uma coluna. É `SUM(delta)` sobre `lancamento`, que é append-only.
--    Com coluna, um bug de aposta deixa saldo errado e sem rastro de onde errou; com
--    ledger, o saldo é sempre explicável — dá para listar de onde veio cada moeda.
--    As 1000 moedas iniciais são o primeiro lançamento, não um DEFAULT.
--
-- 2. Nenhuma tabela guarda carta. As cartas vivem só em memória, na mesa. Persistir mão
--    daria um lugar de onde vazar, e v1 não promete retomar partida interrompida.

CREATE TABLE jogador (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    -- NOCASE para que "Ana" e "ana" não sejam dois jogadores: o apelido é identidade.
    apelido     TEXT NOT NULL UNIQUE COLLATE NOCASE,
    senha_hash  TEXT NOT NULL,
    criado_em   TEXT NOT NULL
);

CREATE TABLE sessao (
    token       TEXT PRIMARY KEY,
    jogador_id  INTEGER NOT NULL REFERENCES jogador(id) ON DELETE CASCADE,
    criada_em   TEXT NOT NULL,
    expira_em   TEXT NOT NULL
);
CREATE INDEX idx_sessao_jogador ON sessao(jogador_id);

CREATE TABLE partida (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    modo              TEXT NOT NULL,
    aposta            INTEGER NOT NULL,
    estado            TEXT NOT NULL,   -- aguardando | em_curso | encerrada
    equipe_vencedora  INTEGER,
    criada_em         TEXT NOT NULL,
    encerrada_em      TEXT
);

CREATE TABLE participacao (
    partida_id  INTEGER NOT NULL REFERENCES partida(id),
    jogador_id  INTEGER NOT NULL REFERENCES jogador(id),
    assento     INTEGER NOT NULL,
    PRIMARY KEY (partida_id, assento)
);
CREATE INDEX idx_participacao_jogador ON participacao(jogador_id);

CREATE TABLE lancamento (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    jogador_id  INTEGER NOT NULL REFERENCES jogador(id),
    delta       INTEGER NOT NULL,
    motivo      TEXT NOT NULL,
    partida_id  INTEGER REFERENCES partida(id),
    criado_em   TEXT NOT NULL
);
CREATE INDEX idx_lancamento_jogador ON lancamento(jogador_id);

CREATE TABLE webhook (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    jogador_id  INTEGER NOT NULL REFERENCES jogador(id) ON DELETE CASCADE,
    url         TEXT NOT NULL,
    -- Segredo por registro: o integrador verifica a assinatura HMAC e sabe que o evento
    -- veio daqui, e não de quem descobriu a URL dele.
    segredo     TEXT NOT NULL,
    criado_em   TEXT NOT NULL
);
CREATE INDEX idx_webhook_jogador ON webhook(jogador_id);

CREATE TABLE entrega (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    webhook_id  INTEGER NOT NULL REFERENCES webhook(id) ON DELETE CASCADE,
    evento      TEXT NOT NULL,
    status      INTEGER,
    erro        TEXT,
    criado_em   TEXT NOT NULL
);
CREATE INDEX idx_entrega_webhook ON entrega(webhook_id);
