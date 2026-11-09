# Spike (c): fidelidade e custo do Slint

- **Data:** 2026-11-09
- **Código (descartável):** branch `spike/fase-0`, pasta `spikes/slint-stage/`
- **Pergunta:** o Slint reproduz o palco da Visão geral do design (ilustração em linha, grade, chamadas, animação) com qualidade, e quanto custa em RAM e tempo de abertura?

## Resposta curta

**Sim.** Com o **renderizador por software**, a tela completa abre em ~6 ms, ocupa ~25 MB de working set e usa ~0,5% de CPU animando a 60 Hz, com aparência praticamente idêntica ao renderizador por GPU. **Recomendação: Slint com `renderer-software` como padrão do `axis-ui`.** O Tauri não é necessário.

## O que foi construído

Janela de 1280×800 com barra de título, barra lateral (navegação e 3 cartões de controle com cor do dono e Player), título, palco com grade de 24 px, ilustração do controle em linha (um `Path` + retângulos), chamadas com linhas pontilhadas e valores ao vivo, painel de métricas, plots dos sticks, barras de gatilho e pílulas da cadeia de ajustes. A entrada é simulada por um timer de 16 ms (60 Hz) que altera 7 propriedades.

Capturas: `spikes/slint-stage/screenshot.png` (GPU, femtovg) e `screenshot-software.png` (software).

## Medições (release, Intel Core Ultra 9 285K, Windows 11)

| Renderizador | Working set | Memória privada | CPU animando a 60 Hz | Janela criada | Loop de eventos ativo |
|---|---|---|---|---|---|
| **software** | **25,1 MB** | **8,2 MB** | **0,5%** | **3,7 ms** | **6,1 ms** |
| femtovg (OpenGL) | 95–166 MB | 124–181 MB | 1,0–2,1% | 124 ms | 127–140 ms |

- A meta da spec 9 (abrir em < 500 ms) é atendida com folga pelos dois. Só o software fica na ordem de grandeza de um app "leve".
- O custo do femtovg é quase todo o driver OpenGL carregado no processo, não o Slint.
- O "tempo até o loop de eventos" é medido dentro do processo (de `main` ao primeiro timer). Não inclui o carregamento do executável pelo Windows.
- CPU medida em 2 s com `TotalProcessorTime`, dividida pelo número de núcleos.

## Fidelidade ao design

**Reproduzido bem:** paleta quente com acento `#d97757`, painéis com raio de 18 px e bordas finas, grade, ilustração em linha com curvas suaves, pílulas, números tabulares, animação fluida dos sticks e gatilhos, brilho (drop shadow) nos pontos.

**Lacunas encontradas:**
1. **Linha tracejada:** o `Path` do Slint não tem tracejado. As chamadas usam uma fileira de retângulos de 2 px, o que funciona.
2. **Fontes:** o spike usou Bahnschrift e Segoe UI do sistema. No app, embutir **Barlow Condensed e Barlow** com `import "fonts/….ttf"` no `.slint`. Precisa baixar as fontes do Google Fonts (licença OFL).
3. **Glifo `✕` ausente** na fonte, aparecendo como um quadrado. Desenhar os símbolos de face (△ ○ □ ✕) como `Path`, não como texto.
4. **Layout das chamadas:** em 1280 px as chamadas laterais ficam cortadas pela borda do palco. O palco precisa de layout responsivo (escalar a ilustração e posicionar as chamadas por âncora), que é trabalho de design no plano da UI.
5. **Renderizador por software:** a borda superior de um painel com `clip: true` e cantos arredondados some em um ponto. É uma pequena falha de renderização a contornar (não usar `clip` no painel, ou pôr a borda num retângulo por cima).
6. **Cores no macro `slint!`:** literais como `#31302e` quebram o tokenizador do Rust (`2e` vira expoente). Usar arquivos `.slint` com `slint-build`, como já fazemos.

## Decisões para o plano da UI

- `axis-ui` usa Slint 1.18 com `default-features = false` + `backend-winit` + `renderer-software`, e fontes embutidas.
- O femtovg fica disponível por feature, caso surja necessidade de efeitos pesados (blur grande, muitas camadas).
- Os ícones e símbolos do controle são desenhados como `Path`.
- O brilho que segue o mouse (pós-MVP) deve ser testado no renderizador por software antes de entrar.
