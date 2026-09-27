Host: Intel(R) Xeon(R) Processor @ 2.10GHz; 5 runs after 1 warm-up, median (min–max); cycles at 2.1 GHz nominal.

| config | kernel | ns/access | cycles/access |
|---|---|---:|---:|
| sv39 | chase W=8 | 4.15 (3.98–4.20) | 8.7 |
| sv39 | stream W=8 | 0.34 (0.26–0.49) | 0.7 |
| sv39 | chase W=4096 | 28.09 (27.60–34.98) | 59.0 |
| sv39 | stream W=4096 | 28.10 (27.18–29.07) | 59.0 |
| direct | chase W=8 | 1.01 (0.78–1.02) | 2.1 |
| direct | stream W=8 | 0.01 (-0.00–0.04) | 0.0 |
| direct | chase W=4096 | 16.31 (14.68–20.49) | 34.3 |
| direct | stream W=4096 | 2.56 (2.41–3.37) | 5.4 |
| softmmu | chase W=8 | 3.27 (3.24–4.18) | 6.9 |
| softmmu | stream W=8 | 0.32 (0.31–0.54) | 0.7 |
| softmmu | chase W=4096 | 17.07 (15.86–21.02) | 35.8 |
| softmmu | stream W=4096 | 13.76 (13.29–17.33) | 28.9 |
