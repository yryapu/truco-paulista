// Teste de front num navegador real, com um contexto por jogador.
//
// Por que Playwright, e não jsdom/Cypress/Selenium: ver decisão D-12 no repositório de
// pesquisa. O resumo é que truco precisa de 2 a 4 jogadores SIMULTÂNEOS, cada um com cookie
// próprio, e `browser.newContext()` é exatamente essa primitiva. É o que permite **provar** o
// isolamento em vez de afirmá-lo.

const { test, expect } = require('@playwright/test');

// Apelido único por execução: o servidor é compartilhado e tem banco persistente.
const sufixo = () => Math.random().toString(36).slice(2, 8);

/**
 * Aposta única por teste — e isto não é detalhe de arrumação, é isolamento de teste.
 *
 * O pareamento do servidor junta quem quer o **mesmo modo e a mesma aposta**. Com apostas
 * repetidas entre testes, um teste pode sentar na mesa deixada por outro (ou por uma execução
 * anterior que abortou, já que o banco persiste), e aí o jogador cai no assento errado, o
 * parceiro fica esperando sozinho, e a falha aparece longe da causa. Foi o que me aconteceu: o
 * teste do Unicode falhou dentro da suíte e passou isolado, que é a assinatura de contaminação.
 *
 * Em vez de limpar estado antes de cada teste, uso a chave de pareamento a meu favor: aposta
 * distinta = mesa que ninguém mais procura. Fica independente por construção, não por faxina.
 */
const BASE_DA_APOSTA = 100 + Math.floor(Math.random() * 800);
let contador = 0;
const apostaUnica = () => BASE_DA_APOSTA + contador++;

/** Abre um navegador isolado e cadastra um jogador novo. Devolve página e apelido. */
async function novoJogador(browser, prefixo) {
  const contexto = await browser.newContext();
  const page = await contexto.newPage();
  const apelido = `${prefixo}${sufixo()}`;
  await page.goto('/');
  await page.getByTestId('in-apelido').fill(apelido);
  await page.getByTestId('in-senha').fill('segredo123');
  await page.getByTestId('btn-cadastrar').click();
  await expect(page.getByTestId('apelido-logado')).toHaveText(apelido);
  return { contexto, page, apelido };
}

async function sentar(page, modo, aposta) {
  await page.getByTestId('modo').selectOption(modo);
  await page.getByTestId('aposta').fill(String(aposta));
  await page.getByTestId('btn-sentar').click();
}

/** As cartas que estão na mão do jogador, como caracteres. */
async function cartasNaMao(page) {
  return page.getByTestId('sua-mao').locator('button').evaluateAll(
    (bs) => bs.map((b) => b.dataset.carta));
}

const CARTAS_DO_TRUCO = new Set([
  ...'🂤🂥🂦🂧🂭🂫🂮🂡🂢🂣', ...'🂴🂵🂶🂷🂽🂻🂾🂱🂲🂳',
  ...'🃄🃅🃆🃇🃍🃋🃎🃁🃂🃃', ...'🃔🃕🃖🃗🃝🃛🃞🃑🃒🃓',
].filter((c) => c.codePointAt(0) >= 0x1f0a0));

/**
 * Joga um lance em cada página que tiver algo a fazer. Devolve true se a partida acabou.
 * O bot não joga bem; joga **legal**, que é do que o teste precisa.
 */
async function umLance(paginas) {
  for (const p of paginas) {
    if (await p.getByTestId('btn-onze-jogar').isVisible()) {
      await p.getByTestId('btn-onze-jogar').click();
      return false;
    }
    if (await p.getByTestId('btn-aceitar').isVisible()) {
      await p.getByTestId('btn-aceitar').click();
      return false;
    }
    const aviso = (await p.getByTestId('aviso').textContent()) || '';
    // Qualquer das duas telas finais encerra o laço: a de quem ganhou e a de quem perdeu.
    if (aviso.includes('venceram') || aviso.includes('partida acabou')) return true;
    if (aviso.includes('sua vez')) {
      const botoes = p.getByTestId('sua-mao').locator('button:not([disabled])');
      if ((await botoes.count()) > 0) {
        await botoes.first().click();
        return false;
      }
    }
  }
  return false;
}

/**
 * Joga até alguém vencer, com orçamento em **tempo de parede**, não em iterações.
 *
 * Minha primeira versão orçava 400 iterações, e falhou por um motivo que vale registrar: o
 * custo dominante de uma partida não são os lances, é a **pausa de 2,6 s entre mãos** (a que
 * existe para o jogador ver o resultado). Cada pausa queima ~43 iterações sem nada a fazer, e
 * uma partida de 1x1 até 12 pontos leva ~12 mãos — ou seja, as pausas sozinhas consumiam o
 * orçamento inteiro antes de a partida acabar. Orçamento de iteração não é orçamento de tempo
 * quando o laço passa a maior parte dele esperando.
 *
 * Mantenho a pausa real de produção em vez de encurtá-la por variável de ambiente: um e2e que
 * roda com outra configuração de tempo não testa o que o jogador vai usar.
 */
async function jogarAteOFim(paginas, limiteMs = 150_000) {
  const prazo = Date.now() + limiteMs;
  let lances = 0;
  while (Date.now() < prazo) {
    if (await umLance(paginas)) return lances;
    lances++;
    await paginas[0].waitForTimeout(40);
  }
  throw new Error(`a partida nao terminou em ${limiteMs / 1000}s (${lances} lances)`);
}

test('cadastro: do zero a jogavel, com 1000 moedas e emblemas de estreante', async ({ browser }) => {
  const inicio = Date.now();
  const { contexto, page } = await novoJogador(browser, 'ana');

  // O requisito do enunciado é "comecar a jogar em menos de um minuto".
  await expect(page.getByTestId('btn-sentar')).toBeVisible();
  const segundos = (Date.now() - inicio) / 1000;
  expect(segundos, `cadastro levou ${segundos.toFixed(1)}s`).toBeLessThan(60);

  await expect(page.getByTestId('saldo')).toHaveText('1000');
  await expect(page.getByTestId('emblema-vitorias')).toHaveText('Entrando na Mesa');
  await expect(page.getByTestId('emblema-historico')).toHaveText('Sem Histórico');
  await contexto.close();
});

test('1x1: a carta na tela e o caractere Unicode, e a do adversario e um dorso', async ({ browser }) => {
  const a = await novoJogador(browser, 'uni_a');
  const b = await novoJogador(browser, 'uni_b');
  const aposta = apostaUnica();
  await sentar(a.page, '1x1', aposta);
  await sentar(b.page, '1x1', aposta);

  await expect(a.page.getByTestId('sua-mao').locator('button')).toHaveCount(3);
  await expect(b.page.getByTestId('sua-mao').locator('button')).toHaveCount(3);

  // O pareamento juntou estes dois, e não um deles com sobra de outra mesa. Asserção explícita
  // para que uma falha de pareamento acuse pareamento, em vez de aparecer como carta faltando.
  await expect(a.page.getByTestId('assento-1')).toContainText(b.apelido);
  await expect(b.page.getByTestId('assento-0')).toContainText(a.apelido);

  // (1) o que está na mão é carta do baralho de truco, um caractere cada
  const maoA = await cartasNaMao(a.page);
  expect(maoA).toHaveLength(3);
  for (const c of maoA) {
    expect([...c], `${c} deveria ser um unico caractere`).toHaveLength(1);
    expect(CARTAS_DO_TRUCO.has(c), `${c} nao e carta do truco`).toBe(true);
  }
  // a vira também
  const vira = await a.page.getByTestId('vira').textContent();
  expect(CARTAS_DO_TRUCO.has(vira), `vira ${vira} invalida`).toBe(true);
  await expect(a.page.getByTestId('manilha')).not.toHaveText('—');

  // (2) do adversário aparece dorso, não carta
  const dorsos = await a.page.getByTestId('assento-1').locator('.dorsos').textContent();
  expect(dorsos).toBe('🂠🂠🂠');

  // (3) ISOLAMENTO, medido no DOM: nenhuma carta de b aparece no HTML de a
  const maoB = await cartasNaMao(b.page);
  const htmlA = await a.page.content();
  for (const c of maoB) {
    if (maoA.includes(c) || c === vira) continue; // impossível, mas não quero falso positivo
    expect(htmlA.includes(c), `a carta ${c} de b vazou para o DOM de a`).toBe(false);
  }
  await a.contexto.close();
  await b.contexto.close();
});

test('1x1: partida inteira pela interface, com saldo e ranking no fim', async ({ browser }) => {
  // Uma partida real inclui ~12 pausas de 2,6 s entre mãos. O prazo tem de caber nisso.
  test.setTimeout(240_000);
  const a = await novoJogador(browser, 'jogo_a');
  const b = await novoJogador(browser, 'jogo_b');
  const aposta = apostaUnica();
  await sentar(a.page, '1x1', aposta);
  await sentar(b.page, '1x1', aposta);
  await expect(a.page.getByTestId('sua-mao').locator('button')).toHaveCount(3);

  // A aposta já saiu do saldo quando a mesa encheu.
  await a.page.getByTestId('btn-lobby').click();
  await expect(a.page.getByTestId('saldo')).toHaveText(String(1000 - aposta));
  // volta para a mesa
  await sentar(a.page, '1x1', aposta);

  await jogarAteOFim([a.page, b.page]);

  // Exatamente uma das duas telas anuncia vitória, e a outra anuncia derrota.
  //
  // Cuidado que me custou uma execução: "eles venceram" e "vocês venceram" **compartilham** a
  // palavra `venceram`, então `includes('venceram')` dava true nas duas telas e acusava
  // incoerência onde o jogo estava coerente. Asserção tem de casar com o que distingue os dois
  // desfechos, não com o que eles têm em comum.
  const avisoA = await a.page.getByTestId('aviso').textContent();
  const avisoB = await b.page.getByTestId('aviso').textContent();
  const venceA = avisoA.includes('vocês venceram');
  const venceB = avisoB.includes('vocês venceram');
  expect(venceA !== venceB, `avisos incoerentes: "${avisoA}" / "${avisoB}"`).toBe(true);
  // E o perdedor vê a derrota dita, não uma tela ambígua.
  const perdeu = venceA ? avisoB : avisoA;
  expect(perdeu).toContain('eles venceram');

  // R-09: quem venceu chegou a 12.
  const vencedor = venceA ? a.page : b.page;
  const pontos = Number(await vencedor.getByTestId('pontos-nos').textContent());
  expect(pontos, 'R-09: vence quem faz 12 pontos').toBeGreaterThanOrEqual(12);

  // Saldo: o vencedor recebe 2x a aposta, o perdedor fica sem ela.
  const perdedor = venceA ? b.page : a.page;
  await vencedor.getByTestId('btn-lobby').click();
  await expect(vencedor.getByTestId('saldo')).toHaveText(String(1000 + aposta));
  await perdedor.getByTestId('btn-lobby').click();
  await expect(perdedor.getByTestId('saldo')).toHaveText(String(1000 - aposta));

  // Emblema de reputação mudou com a vitória.
  await expect(vencedor.getByTestId('emblema-vitorias')).toHaveText('Primeira Mão');
  await expect(perdedor.getByTestId('emblema-vitorias')).toHaveText('Entrando na Mesa');

  // E o ranking mostra o vencedor com os dois emblemas.
  const nomeVencedor = venceA ? a.apelido : b.apelido;
  const linha = vencedor.getByTestId('ranking').locator('tr', { hasText: nomeVencedor }).first();
  await expect(linha).toContainText('Primeira Mão');
  await expect(linha).toContainText('Estreante');

  await a.contexto.close();
  await b.contexto.close();
});

test('truco pela interface: quem pede ganha 1 ponto quando o outro corre', async ({ browser }) => {
  const a = await novoJogador(browser, 'truc_a');
  const b = await novoJogador(browser, 'truc_b');
  const aposta = apostaUnica();
  await sentar(a.page, '1x1', aposta);
  await sentar(b.page, '1x1', aposta);
  await expect(a.page.getByTestId('sua-mao').locator('button')).toHaveCount(3);

  // R-07: só se pede na própria vez, então o pedinte é quem tem a vez.
  const temAVez = (await a.page.getByTestId('aviso').textContent()).includes('sua vez');
  const pedinte = temAVez ? a.page : b.page;
  const corredor = temAVez ? b.page : a.page;

  await expect(pedinte.getByTestId('btn-truco')).toBeVisible();
  await expect(pedinte.getByTestId('btn-truco')).toHaveText('TRUCO!');
  // O outro não pode pedir: não é a vez dele.
  await expect(corredor.getByTestId('btn-truco')).toBeHidden();

  await pedinte.getByTestId('btn-truco').click();

  // O desafiado vê as três respostas; o pedinte, nenhuma.
  await expect(corredor.getByTestId('btn-aceitar')).toBeVisible();
  await expect(corredor.getByTestId('btn-correr')).toBeVisible();
  await expect(corredor.getByTestId('btn-aumentar')).toHaveText('SEIS!');
  await expect(corredor.getByTestId('aviso')).toContainText('pediram truco');
  await expect(pedinte.getByTestId('aviso')).toContainText('pediu truco');
  await expect(pedinte.getByTestId('btn-aceitar')).toBeHidden();

  // A mão já vale 3 na proposta... mas só conta se aceitarem. Correr dá 1 (R-06).
  await corredor.getByTestId('btn-correr').click();
  await expect(pedinte.getByTestId('pontos-nos')).toHaveText('1');
  await expect(corredor.getByTestId('pontos-eles')).toHaveText('1');
  await expect(corredor.getByTestId('pontos-nos')).toHaveText('0');
  await expect(pedinte.getByTestId('log')).toContainText('TRUCO');

  await a.contexto.close();
  await b.contexto.close();
});

test('2x2: quatro jogadores, duplas alternadas e dorsos para todos', async ({ browser }) => {
  const js = [];
  for (const n of ['d1_', 'd2_', 'd3_', 'd4_']) js.push(await novoJogador(browser, n));
  const aposta = apostaUnica();
  for (const j of js) await sentar(j.page, '2x2', aposta);

  for (const j of js) {
    await expect(j.page.getByTestId('sua-mao').locator('button')).toHaveCount(3);
    // Três outros assentos na mesa, cada um com três dorsos.
    await expect(j.page.getByTestId('adversarios').locator('.assento')).toHaveCount(3);
  }
  // Um parceiro e dois adversários, do ponto de vista de cada um.
  const rotulos = await js[0].page.getByTestId('adversarios')
    .locator('.assento .eq').evaluateAll((es) => es.map((e) => e.textContent).sort());
  expect(rotulos).toEqual(['adversário', 'adversário', 'parceiro']);

  // As doze cartas em jogo são distintas: ninguém recebeu a carta de ninguém.
  const todas = [];
  for (const j of js) todas.push(...(await cartasNaMao(j.page)));
  expect(new Set(todas).size, 'as quatro maos tem de ser disjuntas').toBe(12);

  for (const j of js) await j.contexto.close();
});

test('webhook: registrar mostra o segredo uma vez e lista a URL', async ({ browser }) => {
  const { contexto, page } = await novoJogador(browser, 'hook');
  await page.getByTestId('in-webhook').fill('https://exemplo.invalid/truco-hook');
  await page.getByTestId('btn-webhook').click();

  const segredo = page.getByTestId('segredo-webhook');
  await expect(segredo).toBeVisible();
  await expect(segredo).toContainText('whsec_');
  await expect(segredo).toContainText('HMAC-SHA256');
  await expect(page.getByTestId('lista-webhooks')).toContainText('https://exemplo.invalid/truco-hook');

  // URL de metadados de nuvem é recusada (mitigação de SSRF).
  await page.getByTestId('in-webhook').fill('http://169.254.169.254/latest/meta-data/');
  await page.getByTestId('btn-webhook').click();
  await expect(page.getByTestId('erro-webhook')).toContainText('link-local');

  await contexto.close();
});

test('sessao: o lobby nao e alcancavel sem login, e logout volta para a entrada', async ({ browser }) => {
  const contexto = await browser.newContext();
  const page = await contexto.newPage();
  await page.goto('/');
  // Sem cookie, a tela de entrada é a única visível.
  await expect(page.getByTestId('btn-cadastrar')).toBeVisible();
  await expect(page.getByTestId('btn-sentar')).toBeHidden();

  // Credencial errada não entra, e a mensagem não diz qual dos dois campos falhou.
  await page.getByTestId('in-apelido').fill(`fantasma${sufixo()}`);
  await page.getByTestId('in-senha').fill('senhaerrada');
  await page.getByTestId('btn-entrar').click();
  await expect(page.getByTestId('erro-entrada')).toHaveText('apelido ou senha incorretos');

  await contexto.close();

  const { contexto: c2, page: p2 } = await novoJogador(browser, 'saida');
  await p2.getByTestId('btn-logout').click();
  await expect(p2.getByTestId('btn-cadastrar')).toBeVisible();
  await expect(p2.getByTestId('btn-sentar')).toBeHidden();
  // E recarregar não devolve a sessão: o cookie foi revogado no servidor.
  await p2.reload();
  await expect(p2.getByTestId('btn-cadastrar')).toBeVisible();
  await c2.close();
});
