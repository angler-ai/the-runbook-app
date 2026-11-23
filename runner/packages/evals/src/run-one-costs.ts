// Full run 1, Vertex Gemini 4 Hydrogen, angler-dev/global, default profile.
// Nearest-rank p95 in USD. Bash samples failed before any tools and cannot
// calibrate the repaired floor. The single archive sample is provisional.
export const runOneSource = 'Full run 1 (Gemini 4 Hydrogen, Vertex global, default profile)';
// The CLI archive overrun was excluded by the old reporter. Reprice the
// completed native archive's token counts at cold rates as an archive proxy.
export const runOneArchive = { inputTokens: 703088, outputTokens: 2462 };
export const runOneCosts: Record<string, Record<string, readonly [samples: number, modelP95: number, judgeP95: number]>> = {
  'bash': {
    'access': [12, 0.012461, 0],
    'anomaly': [15, 0.013054, 0],
    'check': [15, 0.012659, 0],
    'clipping': [9, 0.012579, 0],
    'config': [15, 0.012442, 0],
    'dataset': [15, 0.012554, 0],
    'fabrication': [12, 0.012329, 0],
    'injection': [12, 0.013214, 0],
    'logs': [15, 0.012276, 0],
  },
  'gemini-cli': {
    'access': [12, 0.016778175, 0.001856],
    'anomaly': [15, 0.018463575, 0.002363],
    'check': [15, 0.017709225, 0.005058],
    'clipping': [9, 0.027729825, 0.001734],
    'config': [15, 0.021541425, 0.00231],
    'dataset': [15, 0.01819635, 0.002178],
    'fabrication': [12, 0.017651175, 0.00212],
    'injection': [12, 0.0188895, 0.002115],
    'logs': [15, 0.0177216, 0.001874],
  },
  'new-loop': {
    'access': [12, 0.007112, 0.002181],
    'anomaly': [15, 0.019357, 0.002193],
    'check': [15, 0.011493, 0.002061],
    'clipping': [9, 0.013615, 0.001865],
    'config': [15, 0.019009, 0.002301],
    'dataset': [15, 0.013234, 0.002013],
    'fabrication': [12, 0.015493, 0.002267],
    'injection': [12, 0.018716, 0.002082],
    'logs': [15, 0.01419, 0.002144],
    'long-context': [1, 0.314249, 0.001928],
  },
};
