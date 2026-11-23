export type ClaudeEffort = 'low' | 'medium' | 'high';

export function claudeSettings(provider: string, model: string, profile: string, effort?: ClaudeEffort) {
  const id = model.replace(/(?:@|-)[0-9]{8}$/, '');
  const claude = provider === 'anthropic' || provider === 'vertex';
  const latest = /^claude-(?:(?:fable|mythos)-5(?:-[12])?|opus-5(?:-5)?|sonnet-5-5|haiku-5-5|opus-4-8)$/.test(id);
  const adaptive = latest || /^claude-(?:sonnet-5|opus-4-[67]|sonnet-4-6)$/.test(id);
  const updates = /^claude-(?:(?:fable|mythos)-5(?:-[12])?|opus-5-5|sonnet-5-5)$/.test(id) && id !== 'claude-mythos-5';
  const requestedEffort = effort === 'low' || effort === 'medium' || effort === 'high' ? effort : undefined;
  const contextEditing = claude && /^claude-(?:opus|sonnet|haiku|fable|mythos)-[45](?:-|$)/.test(id);
  return {
    anthropic: claude,
    midConversationSystem: claude && latest,
    taskBudget: claude && (latest || id === 'claude-opus-4-7'),
    progressUpdates: claude && updates,
    contextEditing,
    modelOptions: {
      ...(claude && (adaptive || id === 'claude-opus-4-5') && {
        effort: requestedEffort ?? (profile === 'cheap' ? 'low' : profile === 'deep' ? 'high' : 'medium'),
      }),
      ...(claude && adaptive && { thinking: { type: 'adaptive' as const,
        display: updates ? 'updates' as const : 'summarized' as const } }),
      ...(contextEditing && { contextManagement: { edits: [
        { type: 'clear_thinking_20251015' as const, keep: { type: 'thinking_turns' as const, value: 2 } },
        { type: 'clear_tool_uses_20250919' as const, trigger: { type: 'input_tokens' as const, value: 100_000 },
          keep: { type: 'tool_uses' as const, value: 3 }, clearAtLeast: { type: 'input_tokens' as const, value: 20_000 } },
      ] } }),
    },
  };
}
