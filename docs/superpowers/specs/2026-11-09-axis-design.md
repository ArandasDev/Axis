# Axis: especificação de design

- **Data:** 2026-11-09
- **Status:** aprovado em conversa, aguardando revisão da spec escrita
- **Plataforma inicial:** Windows 10/11 (x64)
- **Uso:** pessoal (o autor, amigos e família). Não é produto comercial nem projeto aberto à comunidade.

---

## 1. Problema e objetivo

O DS4Windows resolve o básico, mas:

1. **É difícil entender o que foi configurado**, como foi configurado e como o emulador interpreta o controle, mesmo para quem sabe configurar.
2. **Com 2 ou 3 controles fica instável e esquece quem é quem.** No dia seguinte é preciso reconfigurar e reorganizar tudo.
3. **Não ajuda a corrigir defeitos do próprio controle**, como drift, calibração ruim, gatilhos com ruído e conexão instável.
4. **A interface é datada e técnica demais.**

**Objetivo:** um app leve em Rust que faz o DualShock 4 e o DualSense (e, se viável, controles Xbox) funcionarem em qualquer jogo **sem esforço**, lembra de cada controle e do seu dono, mostra com clareza o que acontece com cada entrada e ajuda a corrigir controles com defeito.

### Critérios de sucesso

- Ligar o PC, conectar o controle e jogar **sem abrir o Axis**, com tudo configurado como da última vez.
- Com 3 controles de 3 pessoas, cada um sabe qual é o seu (cor e nome) e nada precisa ser reconfigurado entre sessões.
- Qualquer ajuste aplicado fica visível e compreensível na interface: o que entra, o que cada etapa faz, o que sai.
- Um controle com drift leve passa a ser utilizável depois do assistente de correção.
- As metas de desempenho da seção 9 são atingidas e medidas por benchmark.

### Fora de escopo (por decisão)

- Distribuição pública, instalador assinado, auto-update, licenciamento.
- Emular volantes, manches, arcade sticks ou qualquer saída além de Xbox 360 e DualShock 4.
- Driver em modo kernel próprio.
- Reorganização automática de slots de jogador (ver 5.3).

---

## 2. Decisões principais

| Decisão | Escolha | Alternativas descartadas e motivo |
|---|---|---|
| Linguagem | Rust | Pedido explícito. Segurança de memória, desempenho, sem GC |
| Modelo de processo | **Núcleo sem interface na bandeja + interface sob demanda**, com início automático no login | Processo único (a interface ocupa RAM o tempo todo). Serviço do Windows (instalação burocrática, sem ganho para jogos, que só rodam após o login). Só com o app aberto (quebra o plug and play) |
| Futuro serviço | O núcleo não depende de ser bandeja ou serviço. Pode ser embrulhado como serviço sem reescrita | — |
| Framework de interface | **Slint**, com design tokens em um `global` (no espírito do Tailwind) | Tauri (WebView2 pesado, ~100 a 200 MB, duas linguagens; continua como plano B). egui (redesenho contínuo, visual técnico). iced (menos maduro) |
| Controle virtual | **ViGEmBus** via crate `vigem-client`, atrás da trait `VirtualPad` | Driver próprio (assinatura de kernel inviável). O ViGEm foi descontinuado, mas funciona e está isolado para troca futura |
| Ocultar o controle físico | **HidHide**, atrás de uma abstração própria | — |
| Concorrência | **Uma thread por controle, I/O bloqueante, sem runtime assíncrono** | tokio (peso sem benefício para 1 a 4 dispositivos, e polling/wakeups extras) |
| Núcleo ↔ interface | **Named pipe** do Windows, com o protocolo no crate `axis-protocol` | TCP local (porta, firewall), memória compartilhada (complexidade desnecessária) |
| Persistência | Arquivos **TOML** com `serde`, gravação atômica | SQLite (exagerado para dezenas de registros) |

---

## 3. Arquitetura

### 3.1 Workspace Cargo

```
axis/
├─ crates/
│  ├─ axis-core       lib  tipos do domínio + pipeline de ajustes (sem I/O)
│  ├─ axis-devices    lib  drivers por família: ds4, dualsense, xbox; hotplug; identidade
│  ├─ axis-virtual    lib  trait VirtualPad + impl ViGEm; wrapper HidHide
│  ├─ axis-protocol   lib  mensagens núcleo ↔ interface (serde, versionadas)
│  ├─ axis-daemon     bin  núcleo na bandeja: threads, registros, perfis, servidor IPC
│  └─ axis-ui         bin  interface Slint (só telas, sem regra de negócio)
├─ docs/
└─ benches/ (dentro dos crates que medem)
```

### 3.2 Responsabilidades e dependências

| Crate | Faz | Depende de |
|---|---|---|
| `axis-core` | `InputState` normalizado; parâmetros e execução do pipeline; tipos de perfil e de registro de controle | só `serde` |
| `axis-devices` | trait `DeviceDriver` (parse de report → `InputState`, envio de LED/rumble/player LEDs, leitura de identidade); descoberta e hotplug | `axis-core`, `hidapi` e/ou `windows` |
| `axis-virtual` | trait `VirtualPad` (criar, atualizar, receber rumble do jogo, remover); impl ViGEm; controle do HidHide | `axis-core`, `vigem-client`, `windows` |
| `axis-protocol` | enums de comando, evento e snapshot; versão do protocolo | `axis-core`, `serde` |
| `axis-daemon` | orquestra tudo; dono do estado; grava a config; bandeja; servidor do pipe | todos os anteriores |
| `axis-ui` | telas; cliente do pipe | `axis-protocol`, `slint` |

Regra: **`axis-ui` nunca depende de `axis-devices` nem de `axis-virtual`.** Trocar Slint por Tauri reescreve só `axis-ui`.

### 3.3 Caminho crítico (por controle)

```
[HID físico] → thread do controle (read bloqueante) → parse → pipeline → VirtualPad.update → [jogo]
                                                                   └→ (só se a interface está inscrita) snapshot ≤ 60/s → pipe
```

- **Nenhuma alocação no caminho crítico.** Buffers e estruturas de tamanho fixo são reutilizados.
- **Parâmetros "compilados":** quando um perfil ou uma correção muda, o daemon gera uma struct imutável de parâmetros e a troca atomicamente (`Arc` swap). A thread do controle só lê.
- **Rumble do jogo → controle físico:** callback do ViGEm encaminhado ao driver do dispositivo.
- **Com o controle parado, nada roda:** a thread dorme no `read`. Hotplug é por notificação do Windows, nunca por polling.

---

## 4. Dispositivos

### 4.1 Suporte

| Família | Conexão | Entrada | Saída para o controle | Identidade |
|---|---|---|---|---|
| DualShock 4 (v1/v2) | USB, Bluetooth | HID próprio da Sony | lightbar, rumble | MAC (feature report) |
| DualSense | USB, Bluetooth | HID próprio da Sony | lightbar, LEDs de jogador, rumble | MAC (feature report) |
| Xbox One/Series | USB, Bluetooth | **a validar no spike** | — | BT: endereço no caminho do dispositivo; USB: **a validar** |

Requisitos de parse: os reports USB e Bluetooth têm formatos diferentes (offsets, CRC no Bluetooth). Cada driver trata os dois.

### 4.2 Saída virtual

- **MVP:** Xbox 360 (XInput).
- **v0.2:** DualShock 4 virtual, para jogos que se beneficiam do layout PlayStation.

---

## 5. Identidade, memória por controle e perfis

### 5.1 O que pertence a quem

| Pertence ao **controle físico** (`ControllerRecord`) | Pertence ao **perfil** (`Profile`) |
|---|---|
| id (MAC), família, apelido do dono | mapeamento de botões e eixos |
| cor do dono (lightbar) | deadzones (interna, externa, anti-deadzone; radial/por eixo) |
| calibração de analógicos e gatilhos | curvas de resposta |
| compensação de drift | ajustes de gatilho (deadzone, curva, hair trigger) |
| perfil ativo | intensidade de vibração |

Motivo: a correção é sobre o **hardware** e acompanha o controle. A preferência é **intercambiável e exportável** sem levar a calibração de outra pessoa junto.

### 5.2 Armazenamento

```
%APPDATA%\Axis\
├─ settings.toml            preferências do app (tema, destaque, início automático)
├─ controllers\<id>.toml    um por controle físico
└─ profiles\<nome>.toml     perfis
```

- Lido uma vez no início e mantido em memória. Grava só quando algo muda.
- **Gravação atômica:** escreve em um temporário, faz `fsync` e depois renomeia. Mantém um `.bak` da versão anterior.
- Exportar e importar perfil: um arquivo `.axis` (TOML do perfil + versão do formato).
- Cada arquivo tem `schema_version` para migrações futuras.

### 5.3 Fluxo de conexão

1. Notificação de chegada → o daemon abre o dispositivo e identifica a família.
2. Lê a identidade (MAC) → carrega `controllers/<id>.toml`. Se não existir, cria um com o perfil padrão e marca como "novo".
3. Compila os parâmetros (calibração + drift + perfil).
4. Cria o `VirtualPad` e só **depois** oculta o físico (nunca deixar o usuário sem controle).
5. Aplica a cor do dono e os LEDs de jogador conforme o slot real atribuído pelo Windows.
6. Mostra a notificação: "Maria (verde) → Player 2".

**Slots de jogador:** seguem a ordem de conexão. Não há reorganização. A identidade é comunicada pela cor, pelo nome, pelos LEDs de jogador (DualSense) e pela notificação.

### 5.4 Desconexão

- O `VirtualPad` **continua conectado durante um período de graça** (padrão 5 s, configurável) com entrada neutra.
- Se o mesmo id reconectar nesse período, reassume o mesmo `VirtualPad` e, portanto, o mesmo Player.
- Expirado o período, remove o `VirtualPad`.

---

## 6. Pipeline de ajustes

### 6.1 Ordem fixa por analógico

```
cru → calibração → drift → deadzone → curva → remapeamento → saída
```

| Etapa | Parâmetros |
|---|---|
| Calibração | offset de centro (x, y); alcance por direção (4 valores); correção de circularidade |
| Drift | raio de compensação central derivado do ruído medido |
| Deadzone | interna, externa, anti-deadzone; modo radial ou por eixo |
| Curva | linear, exponencial (expoente) ou por pontos (spline monotônica) |
| Remapeamento | tabela entrada → saída (MVP: botão → botão; eixos e combinações na v0.2) |

**Gatilhos:** calibração (repouso, máximo) → deadzone → curva → hair trigger opcional (limiar → digital).

### 6.2 Invariantes (testados por propriedade)

- Toda saída de eixo fica em [−1, 1]; todo gatilho em [0, 1].
- A deadzone e a curva nunca invertem o sinal nem a direção do vetor.
- Entrada dentro da zona interna → saída exatamente 0.
- O pipeline é determinístico: a mesma entrada com os mesmos parâmetros dá a mesma saída.

### 6.3 Modo inspeção (transparência)

Quando a interface está aberta na tela do controle, ela pede um snapshot com **o valor após cada etapa**. A interface mostra o ponto se transformando etapa por etapa e as pílulas de etapa ativas. Sem inscrição, o pipeline não calcula nem envia nada extra.

---

## 7. Kit de correção

Assistentes guiados, cada um com **modo leigo** ("corrigir", um clique) e **modo especialista** (todos os números editáveis). O resultado é salvo no `ControllerRecord`.

| Assistente | Procedimento | Resultado |
|---|---|---|
| Drift | "Solte o analógico" → 3 s de amostragem | nuvem de pontos, desvio do centro, ruído → offset de centro + raio de drift sugerido |
| Alcance e círculo | "Gire devagar até a borda, 3 voltas" | máximo por direção, erro de circularidade em % → calibração |
| Gatilhos | "Aperte até o fim e solte, 3 vezes" | repouso e máximo reais |
| Conexão | passivo, durante o uso | taxa real de reports (Hz), jitter, desconexões, bateria. Sem RSSI (o Windows não expõe de forma confiável) |

**Honestidade:** se a compensação de drift necessária passar de 15% do curso, o Axis avisa que o analógico está desgastado e que limpeza ou troca do módulo resolve de vez.

---

## 8. Interface

### 8.1 Linguagem visual

A base é o projeto do autor no Claude Design: **"AXIS Emulador de Controle"**, página *Controller Emulator v2* (https://claude.ai/artifact/J9GaXwTn8PcR1ctc1jn5bv).

- **Neutros quentes:** fundo `#1c1b1a`, superfície `#262523`, texto `#ecebe6`, divisores com o texto a 15%. Tema claro equivalente.
- **Destaque escolhido pelo usuário,** gerado em OKLCH com luminosidade fixa (0,67) e matiz/croma por opção: laranja (`#d97757`, padrão), vermelho, pink, roxo, índigo, azul, verde. Rampa 100 a 900 derivada.
- **Tipografia:** Barlow Condensed (títulos, 600) e Barlow (texto), embutidas no binário. Números tabulares.
- **Palco:** ilustração em linha do controle **do modelo conectado** (DualSense, DS4, Xbox), grade de 24 px, linhas de chamada pontilhadas com valores ao vivo.
- **Forma:** painéis com raio de 18 px, botões com 11 px, tags em pílula. Microanimações curtas (entrada, indicador de navegação deslizante, avisos).
- **Tokens:** todas as cores, espaçamentos, raios, fontes e durações vêm de um `global Tokens` em Slint. Valores literais em componentes são proibidos.
- **Pós-MVP:** brilho do destaque seguindo o mouse.

### 8.2 Estrutura

- **Barra de título própria:** marca, status do barramento virtual, alternar tema, controles da janela.
- **Barra lateral:** navegação; "Controles" (cartões com cor e nome do dono, modelo, conexão, Player atual, bateria); "Aparência" (claro/escuro + destaque).
- **Visão geral:** palco ao vivo; "Seu controle → O jogo recebe" com pílulas de etapa; métricas (latência, polling, bateria); sticks; gatilhos; registro físico → virtual; "Emular como" (Xbox 360; DS4 a partir da v0.2).
- **Mapeamento:** remapeamento visual sobre a ilustração.
- **Correção:** os assistentes da seção 7.
- **Perfis:** lista, criar, duplicar, renomear, exportar, importar, vincular ao controle.
- **Primeira conexão de um controle novo:** convite para dar nome e escolher a cor.

### 8.3 Bandeja (núcleo)

Menu: abrir o Axis; lista de controles conectados com o Player; pausar ou retomar a emulação; **restaurar controles** (desfaz a ocultação); iniciar com o Windows; sair.

---

## 9. Desempenho

| Métrica | Meta | Como medir |
|---|---|---|
| RAM do núcleo (working set) | ≤ 15 MB com 2 controles | script de medição + Process Explorer |
| CPU do núcleo sem controle ou com controle parado | ~0% (sem wakeups periódicos) | amostragem de CPU por 60 s |
| Latência adicionada (read → submit ao ViGEm), p99 | ≤ 1 ms | timestamps internos, histograma exportado |
| Pipeline por pacote | < 5 µs | `criterion` |
| Abrir a interface (frio) até o primeiro quadro | < 500 ms | timestamp de início → primeiro frame |
| RAM da interface aberta | registrar (sem meta rígida no MVP) | — |

As metas são provisórias. Se uma não for atingível sem prejudicar a estabilidade, revisa-se a meta com dados, não por intuição.

---

## 10. Robustez e tratamento de erros

| Situação | Comportamento |
|---|---|
| ViGEmBus ausente | o daemon detecta; a interface explica e aponta a instalação; **modo visualização** (lê e mostra, não emula) |
| HidHide ausente | emula sem ocultar; a interface avisa sobre "input duplo" possível em alguns jogos |
| O núcleo trava com a ocultação ativa | ocultação só enquanto o núcleo roda; desfaz ao sair; ao iniciar, reconcilia o estado; item **restaurar controles** na bandeja |
| Falha em um controle | contida na sua thread; os outros continuam; o controle afetado é reaberto |
| Config corrompida | tenta o `.bak`; senão usa os padrões e avisa; o arquivo ruim é preservado com o sufixo `.corrupt` |
| Protocolo da interface incompatível com o núcleo | handshake com versão; a interface mostra "atualize/reinicie o Axis" |
| HidHide exige admin | pedir elevação **uma vez** na instalação (configurar a whitelist), nunca a cada boot. **A validar no spike (a).** |

Logs: arquivo rotativo em `%LOCALAPPDATA%\Axis\logs`, nível `info` por padrão, sem escrita no caminho crítico (o evento vai para um canal e é escrito em outra thread).

---

## 11. Testes

- **`axis-core`:** testes unitários e de propriedade (`proptest`) para as invariantes da seção 6.2.
- **`axis-devices`:** parse testado com **reports reais gravados** dos controles do autor (USB e Bluetooth) como fixtures.
- **Gravação e replay:** o daemon pode gravar o fluxo cru de um controle em arquivo. O replay passa pelo pipeline sem hardware, para regressão, benchmark e depuração.
- **`axis-protocol`:** testes de ida e volta da serialização e de compatibilidade de versão.
- **Benchmarks:** `criterion` no pipeline; medição de latência e de recursos com script reproduzível; resultados registrados em `docs/benchmarks/`.
- **Manual (checklist):** conectar e desconectar por USB e Bluetooth, 2 a 3 controles simultâneos, reboot, matar o daemon com a ocultação ativa.

---

## 12. Ambiente de desenvolvimento

- O repositório fica no OneDrive. **`CARGO_TARGET_DIR` aponta para fora do OneDrive** (por exemplo `C:\dev\cargo-target\axis`) para evitar sincronizar GBs de build.
- Git: Conventional Commits, branches curtas (`feat/`, `fix/`, `docs/`…), PR para `main`, autor único (ver `CONTRIBUTING.md`).

---

## 13. Fases

### Fase 0: spikes (código descartável, saída = resposta)
- **(a)** ViGEm + HidHide a partir do Rust: criar Xbox 360 virtual, ocultar e mostrar um DS4, medir a latência básica, descobrir os requisitos de privilégio do HidHide.
- **(b)** Xbox: dá para ler a entrada, ocultar e identificar de forma única por USB e Bluetooth? Decide se o Xbox entra na v0.2.
- **(c)** Slint: reproduzir o palco da Visão geral (ilustração, grade, chamadas, animação) com fidelidade ao design e medir a RAM e o tempo de abertura.

### MVP v0.1
DS4 e DualSense (USB e Bluetooth) → Xbox 360 virtual · identidade e memória por controle · período de graça na desconexão · núcleo na bandeja com início automático · Visão geral com o palco e a cadeia ao vivo · Correção (drift, alcance e círculo, gatilhos) · deadzones, curvas e gatilhos · remapeamento botão → botão · perfis com exportar e importar · tema claro/escuro + destaque · metas de desempenho medidas.

### v0.2
Xbox (se o spike b for positivo) · DS4 virtual como saída · troca automática de perfil por jogo (detecção do processo em foco) · diagnóstico de conexão completo · remapeamento de eixos e combinações.

### Depois
Giroscópio (mira), touchpad como mouse, gatilhos adaptáveis e háptico do DualSense, macros, modo serviço do Windows, brilho seguindo o mouse, Linux.

---

## 14. Riscos

| Risco | Impacto | Mitigação |
|---|---|---|
| ViGEmBus descontinuado deixa de funcionar em uma atualização do Windows | alto | isolado atrás de `VirtualPad`; monitorar alternativas |
| Ocultação do Xbox inviável | médio | o Xbox fica fora até haver solução; o foco é Sony |
| Anti-cheat de alguns jogos bloqueia controles virtuais | médio | documentar; opção de pausar a emulação por controle |
| O Slint não alcança o visual do design | médio | o spike (c) decide cedo; Tauri é o plano B, e o protocolo isolado limita o retrabalho |
| Variações de firmware e clones de DS4 | baixo a médio | parse defensivo; fixtures por variante encontrada |
