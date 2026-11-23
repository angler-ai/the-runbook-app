// Hydrogen accepts thinking levels and rejects numeric thinking budgets.
export const geminiCapabilities = {
  nativeTools: true, parallelTools: true, strictTools: false,
  implicitCaching: true, explicitCaching: false, cacheTtl: false,
  thinking: true, signedThinking: true, effort: true, progressUpdates: false,
  midConversationSystem: false, taskBudget: false, contextEditing: false,
  refusalFallback: false, vision: true,
} as const;

export function geminiThinking(profile: string, effort?: 'low' | 'medium' | 'high') {
  return { thinkingLevel: effort ?? (profile === 'cheap' ? 'low' : profile === 'deep' ? 'high' : 'medium') };
}
