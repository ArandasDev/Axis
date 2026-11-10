# Medições do núcleo (`axis.exe`)

- **Data:** 2026-11-10
- **Máquina:** Intel Core Ultra 9 285K, Windows 11, build release
- **Cenário:** 1 DualShock 4 (clone) por Bluetooth, ~200 reports/s, emulado como Xbox 360 pelo ViGEmBus e oculto pelo HidHide
- **Como:** `Get-Process` (working set, memória privada, `TotalProcessorTime` em ~55 s, dividido pelo número de núcleos) e as linhas `latência p50/p99` que o próprio worker grava no `axis.log` a cada 60 s (de "report recebido" até "`update` do controle virtual retornou")

| Métrica | Parado | Mexendo | Meta (spec 9) |
|---|---|---|---|
| CPU | 0,02% | 0,02% | ~0% |
| Working set | 11,7 MB | 11,7 MB | ≤ 15 MB (com 2 controles) |
| Memória privada | 2,0 MB | 2,0 MB | — |
| Threads | 7 | 7 | — |
| Latência p50 | 10 µs | 20 µs | — |
| Latência p99 | 30 µs | 60 µs | ≤ 1 ms |
| Reports por minuto | 12.008 | 12.000 | — |

Resultado: todas as metas atendidas com folga. A meta de RAM é para 2 controles e foi medida com 1; o custo por controle adicional (uma thread e um virtual) ainda precisa ser medido quando houver dois controles ligados ao mesmo tempo.

## Verificação ponta a ponta (2026-11-10, com o autor)

| Item | Resultado |
|---|---|
| DS4 já conectado ao iniciar → aviso "Controle 1 (verde) → Player 1", lightbar verde | ✅ |
| `joy.cpl` mostra só o "Controlador Xbox 360"; o DS4 físico fica oculto | ✅ |
| Desligar e religar dentro da graça → "Controle 1 reconectou", mesmo Player | ✅ (após a correção do desligamento silencioso no Bluetooth) |
| Desligado além da graça → virtual liberado; ao religar, "→ Player 1" | ✅ |
| Pausar / Retomar emulação | ✅ |
| Restaurar controles → DS4 físico volta a aparecer | ✅ |
| Processo morto à força com o DS4 oculto → próxima inicialização desfaz e reconecta | ✅ |
| `taskkill` sem `/F` e fim da sessão do Windows → encerramento limpo | ✅ |
| Vibração vinda de um jogo | pendente (precisa de um jogo) |
| Dois controles ao mesmo tempo | pendente |

**Achado de hardware corrigido durante a verificação:** no Bluetooth do Windows, desligar o DS4 não fecha nem dá erro no handle HID aberto; as leituras só param, e o mesmo handle volta a funcionar ao religar. O núcleo agora detecta 500 ms de silêncio (controles que transmitem continuamente) como perda de sinal.
