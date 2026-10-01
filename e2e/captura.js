// Script de captura (não é teste): sobe uma mesa 2x2 real e fotografa a tela de um jogador.
// Existe para o repositório mostrar que o jogo é jogável, com as cartas Unicode renderizadas.
const { chromium } = require('@playwright/test');
const BASE = process.env.BASE_URL || 'http://localhost:8411';

(async () => {
  const browser = await chromium.launch();
  const sufixo = Math.random().toString(36).slice(2, 6);
  const paginas = [];
  for (const n of ['mazinho', 'dona_ze', 'tiao', 'nair']) {
    const ctx = await browser.newContext({ locale: 'pt-BR', viewport: { width: 1100, height: 900 } });
    const p = await ctx.newPage();
    await p.goto(BASE);
    await p.getByTestId('in-apelido').fill(`${n}_${sufixo}`);
    await p.getByTestId('in-senha').fill('segredo123');
    await p.getByTestId('btn-cadastrar').click();
    await p.getByTestId('btn-sentar').waitFor();
    await p.getByTestId('modo').selectOption('2x2');
    await p.getByTestId('aposta').fill('120');
    await p.getByTestId('btn-sentar').click();
    paginas.push(p);
  }
  // Espera a mesa encher, e joga algumas cartas para a mesa não ficar vazia na foto.
  await paginas[0].getByTestId('sua-mao').locator('button').first().waitFor();
  for (let i = 0; i < 6; i++) {
    for (const p of paginas) {
      const aviso = (await p.getByTestId('aviso').textContent()) || '';
      if (aviso.includes('sua vez')) {
        const b = p.getByTestId('sua-mao').locator('button:not([disabled])');
        if (await b.count()) { await b.first().click(); break; }
      }
    }
    await paginas[0].waitForTimeout(250);
  }
  // Pede truco de quem tiver a vez, para a foto mostrar o estado mais interessante da mesa.
  for (const p of paginas) {
    if (await p.getByTestId('btn-truco').isVisible()) { await p.getByTestId('btn-truco').click(); break; }
  }
  await paginas[0].waitForTimeout(600);
  await paginas[0].screenshot({ path: '/e2e/mesa-2x2.png', fullPage: true });
  console.log('capturado /e2e/mesa-2x2.png');
  await browser.close();
})();
