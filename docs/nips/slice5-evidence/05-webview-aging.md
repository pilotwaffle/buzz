# Slice 5 - webview aging sample (flags on, timeline open, 2026-09-17)

| time | WebView2 working set (MB) | proc count |
|---|---|---|
| 09:58:41 | 6758 | 26 |
| 10:03:41 | 6807 | 26 |
| 10:08:41 | 6890 | 26 |
| 10:13:42 | 6984 | 26 |
| 10:18:42 | 7076 | 26 |
| 10:23:42 | 4672 | 26 |
| 10:28:42 | 4738 | 26 |
| 10:33:42 | 4834 | 26 |
| 10:38:42 | 4920 | 26 |
| 10:43:43 | 5008 | 26 |
| 10:48:43 | 5091 | 26 |
| 10:53:43 | 5174 | 26 |

**Conclusion (recorded, not asserted — no PRD threshold).** Over 1 h with all four flags on and the timeline open, total WebView2 working set (26 processes, shared across Buzz + other Electron apps) ranged 4.67–7.08 GB. It rose to a 7.08 GB peak (10:18), then GC reclaimed ~2.4 GB to 4.67 GB (10:23), then rose gently to 5.17 GB. The reclamation shows memory is released, not monotonically leaked — the Slice-1 aging risk ("~2 GB and climbing after ~6 h with the timeline open") is NOT reproduced in this window. The figure is total across all WebView2 procs on the machine, not Buzz alone, so absolute values overstate Buzz's footprint. Healthy: peaks-and-reclaims, no unbounded growth.
