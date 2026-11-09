# Benchmark do `axis-core`

- Data: 2026-11-09
- CPU: Intel(R) Core(TM) Ultra 9 285K
- Compilador: rustc 1.99.1 (58cf8f9c3 2026-10-13)
- Perfil de build: `bench` (otimizado), via `cargo bench -p axis-core` (criterion 0.5)
- Cenário: pipeline "realista" (drift 0.06, deadzone radial 0.05/0.95 com anti 0.1, curva `Points` com 4 pontos, hair trigger em R2, 2 botões remapeados) sobre 1024 entradas determinísticas e variadas.

Cada iteração do criterion processa as 1024 entradas; o tempo por entrada é o tempo da iteração dividido por 1024.

| Benchmark | Média por entrada | Limite superior do IC 95% por entrada | Meta |
|---|---|---|---|
| `pipeline_process` | 152 ns (155,78 µs / 1024) | 152 ns (156,07 µs / 1024) | < 5 µs |
| `pipeline_process_traced` | 153 ns (156,97 µs / 1024) | 154 ns (157,30 µs / 1024) | n/a |

A segunda coluna é o limite superior do intervalo de confiança da média reportado pelo criterion, não um p95 real (o criterion não reporta percentis).

Resultado: `Pipeline::process` fica cerca de 33 vezes abaixo da meta de 5 µs (spec 9).
