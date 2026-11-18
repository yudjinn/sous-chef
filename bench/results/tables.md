| Task | Category | Total A | Total B | A/B | In+out A | In+out B | Cache read A | Cache read B | Cache write A | Cache write B | Turns A | Turns B | Calls A | Calls B (sous-chef) | Wall A s | Wall B s | Score A | Score B |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| t01-curl-srcuri | provenance | 258,476 | 113,525 | 2.28 | 7,679 | 4,017 | 222,517 | 98,325 | 28,280 | 11,183 | 15 | 9 | 14 | 8 (8) | 60.4 | 28.9 | 0.8 | 0.9 |
| t02-providers | override | 145,418 | 71,668 | 2.03 | 4,957 | 1,622 | 125,859 | 65,100 | 14,602 | 4,946 | 11 | 7 | 10 | 6 (6) | 33.4 | 11.6 | 0.9 | 0.9 |
| t03-linux-kiss-appends | structure | 120,473 | 171,667 | 0.70 | 6,644 | 3,987 | 101,364 | 155,671 | 12,465 | 12,009 | 22 | 14 | 21 | 13 (13) | 36.0 | 23.0 | 0.75 | 1.0 |
| t04-dropbear-classes | structure | 159,893 | 94,385 | 1.69 | 7,080 | 2,051 | 136,815 | 87,616 | 15,998 | 4,718 | 15 | 6 | 14 | 5 (5) | 40.8 | 19.9 | 0.6 | 0.7 |
| t05-curl-deps | dependencies | 84,639 | 150,350 | 0.56 | 4,042 | 4,920 | 75,895 | 129,499 | 4,702 | 15,931 | 12 | 13 | 11 | 12 (12) | 25.4 | 27.2 | 0.1 | 0.7 |
| t06-gnutls-search | search (control) | 187,336 | 75,549 | 2.48 | 6,270 | 3,230 | 167,664 | 65,923 | 13,402 | 6,396 | 17 | 7 | 16 | 6 (6) | 37.5 | 16.3 | -0.17 | 0.83 |
| t07-busybox-install | task composition | 294,010 | 116,457 | 2.52 | 10,330 | 4,561 | 248,488 | 96,590 | 35,192 | 15,306 | 19 | 12 | 18 | 11 (11) | 52.8 | 33.2 | 0.6 | 1.0 |
| t08-layers | layers | 84,412 | 43,761 | 1.93 | 3,570 | 754 | 69,613 | 43,007 | 11,229 | 0 | 13 | 3 | 12 | 2 (2) | 20.9 | 9.4 | 1.0 | 0.75 |
| t09-lint-meta-kiss | lint | 132,926 | 97,210 | 1.37 | 14,764 | 2,827 | 96,120 | 89,418 | 22,042 | 4,965 | 18 | 8 | 17 | 7 (6) | 74.1 | 25.5 | 0.25 | 0.12 |
| t10-linux-kiss-srcuri | python (uncertain) | 72,780 | 104,876 | 0.69 | 2,902 | 2,363 | 64,330 | 95,573 | 5,548 | 6,940 | 10 | 8 | 9 | 7 (7) | 30.9 | 15.3 | 1.0 | 1.0 |
| t11-distro-features | python (uncertain) | 103,762 | 184,492 | 0.56 | 4,692 | 4,345 | 90,252 | 170,569 | 8,818 | 9,578 | 12 | 14 | 11 | 13 (11) | 28.2 | 28.2 | 0.12 | 1.0 |
| t12-kiss-image-install | override | 182,427 | 74,508 | 2.45 | 7,193 | 3,093 | 157,811 | 64,747 | 17,423 | 6,668 | 22 | 7 | 21 | 6 (6) | 44.5 | 16.6 | 1.0 | 1.0 |
| t13-git-srcrevs | search (control) | 30,596 | 132,358 | 0.23 | 1,670 | 3,232 | 22,724 | 117,205 | 6,202 | 11,921 | 3 | 13 | 2 | 12 (12) | 10.3 | 22.6 | 1.0 | 1.0 |
| t14-u-boot-kiss | provenance | 62,262 | 74,411 | 0.84 | 3,512 | 1,961 | 51,982 | 65,597 | 6,768 | 6,853 | 10 | 7 | 9 | 6 (6) | 21.5 | 17.8 | 0.5 | 0.75 |

| Measure | A sum | B sum | A median | B median |
|---|---|---|---|---|
| total | 1,919,410 | 1,505,217 | 126,700 | 101,043 |
| output | 85,117 | 42,849 | 5,596 | 3,154 |
| num_turns | 199 | 128 | 14 | 8 |
| tool_calls | 185 | 114 | 13 | 7 |
| wall_s | 516.7 | 295.5 | 34.7 | 21.25 |
| cost_usd | 0.09943 | 0.05837 | 0.006737 | 0.003538 |
| score | 8.45 | 11.65 | 0.675 | 0.9 |

Per-task A/B total-token ratio: median 1.53, min 0.23, max 2.52; sum A / sum B 1.28.

| Overhead ('Say ok.') | Total | Cache write | Cache read | Cost USD |
|---|---|---|---|---|
| A | 11,323 | 0 | 11,317 | 0.00011537 |
| B | 20,679 | 0 | 20,673 | 0.00020893000000000002 |

Fixed overhead of the plugin: 9,356 tokens per session start.

Sessions: 31; total tokens 3,484,022; reported cost USD 0.1605.

Tool calls in condition B, all tasks: var 32, search 24, recipe 19, item 14, layers 12, deps 7, Grep 2, slice 1, lint 1, Glob 1, status 1.
Tool calls in condition A, all tasks: Read 70, Grep 61, Bash 32, Glob 30.
