# Spike (a): DS4 → Xbox 360 virtual com ViGEmBus + HidHide

- **Data:** 2026-11-09
- **Código (descartável):** branch `spike/fase-0`, pasta `spikes/ds4-bridge/` (`list`, `read`, `bridge`, `hidehide-status`, `hide`, `unhide`)
- **Ambiente:** Windows 11, ViGEmBus e HidHide instalados pelo autor, processo **não elevado**. Controle: DS4 por Bluetooth (provável clone, ver abaixo).

## Resposta curta

As quatro perguntas foram respondidas e **tudo funciona a partir do Rust, sem privilégio de administrador**:

| Pergunta | Resultado |
|---|---|
| 1. Ler o DS4 e obter identidade | ✅ por Bluetooth: report completo `0x11` a ~200 Hz após ler o feature `0x05`; o MAC vem do serial do HID |
| 2. Criar e alimentar um Xbox 360 virtual | ✅ `vigem-client`: o virtual aparece no slot XInput 0 e o `XInputGetState` devolve exatamente os valores enviados |
| 3. Ocultar o DS4 pelo HidHide | ✅ blacklist + whitelist + ativar por `DeviceIoControl`: um processo não autorizado deixa de ver o DS4; o processo autorizado continua lendo; a restauração volta ao estado original |
| 4. Latência leitura → envio | ✅ **p50 6–10 µs, p99 15–190 µs** (parse + `update` do ViGEm), muito abaixo da meta de 1 ms |

## Detalhes

### Leitura (hidapi 2)
- O DS4 aparece como `VID 054C PID 09CC`, usage `01:05`, bus Bluetooth, produto "Wireless Controller", fabricante "Sony Interactive Entertainment".
- **Modo completo no Bluetooth:** ler o feature report `0x05` faz o controle passar do report reduzido `0x01` para o completo `0x11` (dados a partir do byte 3). Em 5 s: 1 report `0x01` e 1000 reports `0x11`.
- **Taxa observada:** ~200 Hz por Bluetooth com este controle. O DS4 original costuma chegar a ~800–1000 Hz; clones costumam ser mais lentos. Comparar com outro controle e por USB.
- **MAC:** o feature `0x12` (método do USB) falha por Bluetooth com "Parâmetro incorreto", como esperado. Por Bluetooth, o serial do hidapi traz o MAC (`4142000084b6`).
- **Repouso:** sticks em 128/127 (centro de 127,5), botões soltos (d-pad `8`). O byte 7 traz o contador nos bits altos e precisa ser mascarado.

### Clone
O controle se anuncia como DS4 v2 da Sony (`054C:09CC`), mas o MAC `41:42:00:00:84:B6` não pertence à Sony (`41:42` é "AB" em ASCII). Isso indica um clone que copia VID/PID. Consequências:
- A identificação **não pode** depender só de VID/PID para dizer "original".
- A identidade por MAC continua válida, porque é única por controle.
- Recursos finos (giroscópio, lightbar, taxa) devem ser tratados com tolerância.

### Xbox 360 virtual (vigem-client 0.1)
- `Client::connect` → `Xbox360Wired::new` → `plugin` → `wait_ready`. O slot XInput 0 surge em ~300 ms.
- O mapeamento DS4 → XInput usado no spike (d-pad por nibble, ✕→A, ○→B, □→X, △→Y, Share→Back, Options→Start, PS→Guide, Y invertido) foi conferido de ponta a ponta em repouso.
- O centro cru 128 vira `LX 128` no XInput: sem calibração, o "centro" fica levemente fora de zero. O pipeline do `axis-core` (calibração + deadzone) resolve isso.

### HidHide (IOCTLs no `\\.\HidHide`)
- Códigos: `CTL_CODE(0x8001, 2048..2053, METHOD_BUFFERED, FILE_READ_DATA)`: get/set whitelist, get/set blacklist, get/set active.
- As listas são multi-strings UTF-16. A blacklist usa o **instance ID** (`HID\{…}_VID&0002054C_PID&09CC\8&…`), derivado do path do hidapi. A whitelist usa o caminho do executável em formato de dispositivo (`\Device\HarddiskVolume3\…\app.exe`, via `QueryDosDeviceW`).
- **Sem elevação:** abrir o dispositivo, ler e escrever as listas e ativar funcionaram num processo comum. **Não é preciso pedir admin.** A pendência da spec 10 está resolvida.
- Com o DS4 oculto, o processo autorizado continua lendo e espelhando normalmente (p99 17 µs).
- A configuração do HidHide **persiste no driver/registro**. Por isso a regra da spec 10 (ocultar só enquanto o núcleo roda, desfazer ao sair, reconciliar ao iniciar, item "restaurar controles") é obrigatória. O spike restaurou o estado original no fim: inativo, listas vazias.

## Decisões para os planos 3 e 4

- `axis-devices` (DS4): ler o feature `0x05` ao abrir por Bluetooth; tratar os reports `0x01` e `0x11`; MAC pelo feature `0x12` no USB e pelo serial no Bluetooth; mascarar o contador do byte 7.
- `axis-virtual`: `vigem-client` 0.1 atrás de `VirtualPad`; o HidHide é implementado direto (~150 linhas, sem crate), atrás de uma abstração `DeviceHider`.
- O whitelist do HidHide precisa do caminho do executável do **daemon** em formato `\Device\…`.
- Ao ocultar: criar o virtual **antes** de ocultar o físico (spec 5.3) e sempre desfazer ao sair.
- Conversão para i16 do XInput: arredondar, não truncar (pendência da revisão final do plano 1).

## Pendências deste spike

- [ ] **USB:** confirmar o MAC pelo feature `0x12`, a taxa de reports por cabo e o report `0x01` por USB.
- [ ] **Teste com movimento:** comparar valores ao vivo (sticks, gatilhos, botões) entre o DS4 e o XInput. O teste ponta a ponta foi feito em repouso.
- [ ] Comparar a taxa de reports dos três DS4 (verificar se todos são clones).
