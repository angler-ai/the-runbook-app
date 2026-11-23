export interface VertexBinding {
  provider: 'vertex' | 'vertex-gemini';
  project_id: string;
  region: string;
  publisher: 'anthropic' | 'google';
  endpoint: string;
}

export const ATTACHED_VERTEX_PREFIX = 'vertex_attached:';

export function vertexBinding(provider: unknown, project: unknown, region: unknown): VertexBinding | undefined {
  if ((provider !== 'vertex' && provider !== 'vertex-gemini') ||
      typeof project !== 'string' || !/^[a-z][a-z0-9-]{4,28}[a-z0-9]$/.test(project) ||
      typeof region !== 'string' || (region !== 'global' && !/^[a-z]+-[a-z]+[1-9][0-9]*$/.test(region)) ||
      region.length > 40) return undefined;
  const publisher = provider === 'vertex' ? 'anthropic' : 'google';
  const host = region === 'global' ? 'aiplatform.googleapis.com' : `${region}-aiplatform.googleapis.com`;
  return { provider, project_id: project, region, publisher,
    endpoint: `https://${host}/v1/projects/${project}/locations/${region}/publishers/${publisher}/models` };
}

/** Public, canonical destination URLs can look like secrets to the entropy check. */
export function isVertexEndpoint(value: unknown): boolean {
  if (typeof value !== 'string' || value.length > 2048) return false;
  const path = /\/v1\/projects\/([^/]+)\/locations\/([^/]+)\/publishers\/(anthropic|google)\/models$/.exec(value);
  return !!path && vertexBinding(path[3] === 'anthropic' ? 'vertex' : 'vertex-gemini', path[1], path[2])?.endpoint === value;
}
