'use strict';
// Cliente fino. Ele NÃO conhece regra de truco: não sabe o que é manilha, não compara força,
// não decide de quem é a vez. Desenha o estado que chega e manda quatro intenções.
// Essa é a decisão D-11 — o servidor é autoritativo porque há saldo apostado.

const $ = (id) => document.getElementById(id);
const mostrar = (el, v) => el.classList.toggle('oculto', !v);
const NOMES_DE_RODADA = ['1ª', '2ª', '3ª'];

let eu = null;        // perfil
let ws = null;
let mesaId = null;
let ultimo = null;    // último estado recebido

// ---------- HTTP ----------

async function api(rota, metodo = 'GET', corpo) {
  const r = await fetch(rota, {
    method: metodo,
    headers: corpo ? { 'content-type': 'application/json' } : undefined,
    body: corpo ? JSON.stringify(corpo) : undefined,
  });
  const txt = await r.text();
  const dados = txt ? JSON.parse(txt) : {};
  if (!r.ok) throw new Error(dados.erro || `erro ${r.status}`);
  return dados;
}

function erro(el, e) {
  el.textContent = e ? (e.message || String(e)) : '';
}

// ---------- telas ----------

function telas(qual) {
  mostrar($('tela-entrada'), qual === 'entrada');
  mostrar($('tela-lobby'), qual === 'lobby');
  mostrar($('tela-mesa'), qual === 'mesa');
  mostrar($('eu'), qual !== 'entrada');
}

function pintarPerfil(p) {
  eu = p;
  $('apelido-logado').textContent = p.apelido;
  $('saldo').textContent = p.saldo;
  $('emblema-vitorias').textContent = p.emblema_vitorias;
  $('emblema-historico').textContent = p.emblema_historico;
}

async function irParaLobby() {
  telas('lobby');
  await Promise.all([carregarRanking(), carregarWebhooks(), atualizarPerfil()]);
}

async function atualizarPerfil() {
  try { pintarPerfil(await api('/api/eu')); } catch (_) { /* sessão caiu */ }
}

// ---------- entrada ----------

$('form-entrada').addEventListener('submit', async (ev) => {
  ev.preventDefault();
  await autenticar('/api/cadastrar');
});
$('btn-entrar').addEventListener('click', () => autenticar('/api/entrar'));

async function autenticar(rota) {
  erro($('erro-entrada'), null);
  const apelido = $('in-apelido').value.trim();
  const senha = $('in-senha').value;
  try {
    const r = await api(rota, 'POST', { apelido, senha });
    pintarPerfil(r.perfil);
    await irParaLobby();
  } catch (e) {
    erro($('erro-entrada'), e);
  }
}

$('btn-logout').addEventListener('click', async () => {
  if (ws) { ws.close(); ws = null; }
  await api('/api/sair', 'POST').catch(() => {});
  eu = null; mesaId = null;
  telas('entrada');
});

// ---------- lobby ----------

async function carregarRanking() {
  const linhas = await api('/api/ranking');
  $('ranking').replaceChildren(...linhas.map((l) => {
    const tr = document.createElement('tr');
    for (const v of [l.posicao, l.apelido, l.vitorias, l.derrotas]) {
      const td = document.createElement('td');
      td.textContent = v;
      tr.appendChild(td);
    }
    const td = document.createElement('td');
    for (const txt of [l.emblema_vitorias, l.emblema_historico]) {
      const s = document.createElement('span');
      s.className = 'emblema';
      s.textContent = txt;
      td.appendChild(s);
      td.appendChild(document.createTextNode(' '));
    }
    tr.appendChild(td);
    return tr;
  }));
}

async function carregarWebhooks() {
  const hooks = await api('/api/webhooks');
  $('lista-webhooks').replaceChildren(...hooks.map((h) => {
    const li = document.createElement('li');
    const code = document.createElement('code');
    code.textContent = h.url;
    li.appendChild(code);
    const info = document.createElement('span');
    info.textContent = `${h.entregas} entrega(s)` + (h.ultima_falha ? ' · última falhou' : '');
    li.appendChild(info);
    const b = document.createElement('button');
    b.className = 'lisa';
    b.textContent = 'remover';
    b.dataset.testid = `remover-webhook-${h.id}`;
    b.addEventListener('click', async () => {
      await api(`/api/webhooks/${h.id}`, 'DELETE').catch((e) => erro($('erro-webhook'), e));
      await carregarWebhooks();
    });
    li.appendChild(b);
    return li;
  }));
}

$('btn-webhook').addEventListener('click', async () => {
  erro($('erro-webhook'), null);
  try {
    const r = await api('/api/webhooks', 'POST', { url: $('in-webhook').value.trim() });
    // O segredo aparece uma única vez: depois não há como recuperá-lo.
    const cx = $('segredo-webhook');
    cx.textContent = `segredo (guarde agora, não é mostrado de novo):\n${r.segredo}\n\n${r.como_verificar}`;
    mostrar(cx, true);
    $('in-webhook').value = '';
    await carregarWebhooks();
  } catch (e) {
    erro($('erro-webhook'), e);
  }
});

$('btn-sentar').addEventListener('click', async () => {
  erro($('erro-lobby'), null);
  try {
    const r = await api('/api/mesas', 'POST', {
      modo: $('modo').value,
      aposta: Number($('aposta').value),
    });
    mesaId = r.mesa;
    abrirSocket();
    telas('mesa');
  } catch (e) {
    erro($('erro-lobby'), e);
  }
});

$('btn-lobby').addEventListener('click', async () => {
  if (ws) { ws.close(); ws = null; }
  await api('/api/mesas/sair', 'POST').catch(() => {});
  mesaId = null;
  await irParaLobby();
});

// ---------- WebSocket ----------

function abrirSocket() {
  const proto = location.protocol === 'https:' ? 'wss' : 'ws';
  // O cookie de sessão sobe sozinho no handshake por ser mesma origem — não há token na URL.
  ws = new WebSocket(`${proto}://${location.host}/ws?mesa=${mesaId}`);
  ws.addEventListener('message', (ev) => {
    const m = JSON.parse(ev.data);
    if (m.tipo === 'erro') { erro($('erro-mesa'), new Error(m.mensagem)); return; }
    if (m.tipo === 'estado') { ultimo = m; erro($('erro-mesa'), null); pintarMesa(m); }
  });
  ws.addEventListener('close', () => { $('aviso').textContent = 'conexão encerrada'; });
}

function enviar(msg) {
  if (ws && ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify(msg));
}

// ---------- mesa ----------

function carta(ch, extra = '') {
  const d = document.createElement('div');
  d.className = `carta ${extra}`.trim();
  d.textContent = ch;          // o próprio caractere Unicode que veio do servidor
  d.dataset.carta = ch;
  return d;
}

function pintarMesa(e) {
  const souEquipe = e.sua_equipe;
  const nos = souEquipe === null || souEquipe === undefined ? 0 : souEquipe;
  $('pontos-nos').textContent = e.pontos[nos];
  $('pontos-eles').textContent = e.pontos[1 - nos];
  $('vira').textContent = e.vira || '—';
  $('manilha').textContent = e.manilha || '—';
  $('valor').textContent = e.valor || 1;

  // ---- aviso: a linha que diz ao jogador o que está acontecendo
  const f = e.fase;
  let aviso = '';
  if (e.aguardando) {
    aviso = `esperando ${e.faltam} jogador(es)…`;
  } else if (e.vencedora !== null && e.vencedora !== undefined) {
    aviso = e.vencedora === nos ? '🏆 vocês venceram a partida!' : 'a partida acabou — eles venceram';
  } else if (e.especial === 'mao_de_ferro') {
    aviso = '🔒 MÃO DE FERRO — 11 a 11, vale 3, ninguém corre';
  } else if (f && f.fase === 'decidir_onze') {
    aviso = f.equipe === nos
      ? '✋ MÃO DE ONZE — vocês veem as cartas do parceiro: jogar ou correr?'
      : 'mão de onze do adversário — ele está decidindo';
  } else if (f && f.fase === 'respondendo') {
    if (f.responde === e.seu_assento) {
      aviso = `pediram ${nomeDoPedido(e.proposta).toUpperCase()} — aceita, aumenta ou corre?`;
    } else if (f.pedinte === e.seu_assento) {
      // Na própria tela de quem pediu, dizer o nome dele seria estranho.
      aviso = `você pediu ${nomeDoPedido(e.proposta).toUpperCase()} — esperando resposta`;
    } else {
      aviso = `${apelidoDe(e, f.pedinte)} pediu ${nomeDoPedido(e.proposta).toUpperCase()}`;
    }
  } else if (f && f.fase === 'jogando') {
    aviso = f.vez === e.seu_assento ? 'sua vez' : `vez de ${apelidoDe(e, f.vez)}`;
  } else if (f && f.fase === 'encerrada') {
    aviso = f.vencedora === null || f.vencedora === undefined
      ? 'mão empatada — ninguém pontua'
      : `${f.vencedora === nos ? 'vocês' : 'eles'} fizeram ${f.pontos} ponto(s)`;
  }
  $('aviso').textContent = aviso;

  // ---- adversários e parceiro: contagem de cartas, nunca quais
  $('adversarios').replaceChildren(...e.jogadores
    .filter((j) => j.assento !== e.seu_assento)
    .map((j) => {
      const d = document.createElement('div');
      d.className = 'assento' + (f && f.fase === 'jogando' && f.vez === j.assento ? ' vez' : '');
      d.dataset.testid = `assento-${j.assento}`;
      const nome = document.createElement('b');
      nome.textContent = j.apelido;
      d.appendChild(nome);
      const eq = document.createElement('span');
      eq.className = 'eq';
      eq.textContent = j.equipe === nos ? 'parceiro' : 'adversário';
      d.appendChild(eq);
      const visiveis = e.maos_visiveis && e.maos_visiveis[j.assento];
      const dorsos = document.createElement('span');
      dorsos.className = 'dorsos';
      // R-10: na decisão da mão de onze o parceiro mostra as cartas. Fora disso, dorsos.
      dorsos.textContent = visiveis ? visiveis.join(' ') : '🂠'.repeat(j.cartas);
      dorsos.dataset.testid = `cartas-de-${j.assento}`;
      d.appendChild(dorsos);
      return d;
    }));

  // ---- cartas na mesa
  $('na-mesa').replaceChildren(...e.na_mesa.map((j) =>
    carta(j.encoberta ? '🂠' : j.carta, j.encoberta ? 'dorso jogada' : 'jogada')));

  // ---- rodadas ganhas
  $('rodadas').replaceChildren(...e.rodadas.map((r, i) => {
    const s = document.createElement('span');
    s.textContent = `${NOMES_DE_RODADA[i]}: ${r === null ? 'empate' : (r === nos ? 'nós' : 'eles')}`;
    return s;
  }));

  // ---- sua mão
  const minhaVez = f && f.fase === 'jogando' && f.vez === e.seu_assento;
  $('sua-mao').replaceChildren(...e.sua_mao.map((ch) => {
    const b = document.createElement('button');
    b.className = 'carta';
    b.textContent = ch;
    b.dataset.carta = ch;
    b.dataset.testid = `jogar-${ch}`;
    b.disabled = !minhaVez;
    b.addEventListener('click', () => {
      enviar({ acao: 'jogar', carta: ch, encoberta: $('chk-encobrir').checked });
      $('chk-encobrir').checked = false;
    });
    return b;
  }));

  // ---- botões. O cliente só esconde o que o servidor já disse ser impossível;
  //      a recusa de verdade está no motor de regras.
  const respondendo = f && f.fase === 'respondendo' && f.responde === e.seu_assento;
  const decidindoOnze = f && f.fase === 'decidir_onze' && f.decide === e.seu_assento;
  mostrar($('btn-truco'), !!e.pode_pedir);
  $('btn-truco').textContent = rotuloDoPedido(e.valor);
  mostrar($('btn-aceitar'), !!respondendo);
  mostrar($('btn-correr'), !!respondendo);
  mostrar($('btn-aumentar'), !!respondendo && e.proposta < 12);
  $('btn-aumentar').textContent = e.proposta < 12 ? rotuloDoPedido(e.proposta) : 'Aumentar';
  mostrar($('btn-onze-jogar'), !!decidindoOnze);
  mostrar($('btn-onze-correr'), !!decidindoOnze);
  mostrar($('rotulo-encobrir'), !!e.pode_encobrir);

  // ---- log
  $('log').replaceChildren(...e.log.slice(-25).map((ev) => {
    const li = document.createElement('li');
    li.textContent = narrar(e, ev);
    return li;
  }));

  if (e.vencedora !== null && e.vencedora !== undefined) atualizarPerfil();
}

function apelidoDe(e, assento) {
  const j = e.jogadores.find((x) => x.assento === assento);
  return j ? j.apelido : `assento ${assento}`;
}

const NOMES = { 3: 'truco', 6: 'seis', 9: 'nove', 12: 'doze' };
const nomeDoPedido = (v) => NOMES[v] || '?';
const rotuloDoPedido = (valorAtual) => {
  const prox = { 1: 'TRUCO!', 3: 'SEIS!', 6: 'NOVE!', 9: 'DOZE!' };
  return prox[valorAtual] || 'TRUCO!';
};

function narrar(e, ev) {
  const quem = (a) => apelidoDe(e, a);
  const lado = (eq) => (eq === e.sua_equipe ? 'nós' : 'eles');
  switch (ev.evento) {
    case 'mao_iniciada': {
      const extra = ev.especial === 'mao_de_ferro' ? ' — MÃO DE FERRO'
        : (ev.especial && ev.especial.mao_de_onze !== undefined ? ' — mão de onze' : '');
      return `nova mão · vira ${ev.vira} · vale ${ev.valor}${extra}`;
    }
    case 'carta_jogada':
      return ev.encoberta ? `${quem(ev.assento)} jogou encoberta` : `${quem(ev.assento)} jogou ${ev.carta}`;
    case 'rodada_resolvida':
      return `${NOMES_DE_RODADA[ev.numero - 1]} rodada: ${ev.vencedora === null ? 'empate' : lado(ev.vencedora)}`;
    case 'pedido':
      return `${quem(ev.assento)} pediu ${ev.nome.toUpperCase()} (${ev.proposto})`;
    case 'respondido':
      return `${quem(ev.assento)}: ${ev.resposta}`;
    case 'onze_decidida':
      return `mão de onze: ${lado(ev.equipe)} ${ev.aceita ? 'vai jogar' : 'correu'}`;
    case 'mao_encerrada':
      return ev.vencedora === null ? `mão sem ponto — ${ev.motivo}`
        : `${lado(ev.vencedora)} +${ev.pontos} (${ev.motivo})`;
    case 'partida_encerrada':
      return `fim: ${lado(ev.vencedora)} venceram`;
    default:
      return JSON.stringify(ev);
  }
}

$('btn-truco').addEventListener('click', () => enviar({ acao: 'pedir' }));
$('btn-aceitar').addEventListener('click', () => enviar({ acao: 'responder', resposta: 'aceito' }));
$('btn-correr').addEventListener('click', () => enviar({ acao: 'responder', resposta: 'correr' }));
$('btn-aumentar').addEventListener('click', () => enviar({ acao: 'responder', resposta: 'aumentar' }));
$('btn-onze-jogar').addEventListener('click', () => enviar({ acao: 'onze', aceita: true }));
$('btn-onze-correr').addEventListener('click', () => enviar({ acao: 'onze', aceita: false }));

// Sessão viva? Então já vai para o lobby — "menos de um minuto" inclui não logar de novo.
(async () => {
  try {
    pintarPerfil(await api('/api/eu'));
    await irParaLobby();
  } catch (_) {
    telas('entrada');
  }
})();
