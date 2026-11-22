import type { UserAssertionClaims } from '@runbook/runner-shared/contracts/core';

export type Action = 'read' | 'operate' | 'edit' | 'widen' | 'secret' | 'approve';
export type Resource = { kind: 'org' } | { kind: 'agent'; projectId: string };
export type Decision = 'allow' | 'deny' | 'proposal';

export function authorize(claims: UserAssertionClaims, action: Action, resource: Resource): Decision {
  if (action === 'read' ? !claims.scopes.includes('runner:read') : !claims.scopes.includes('runner:admin')) return 'deny';
  if (claims.principal === 'agent' && (action === 'secret' || action === 'approve')) return 'deny';
  const orgAdmin = claims.org_role === 'owner' || claims.org_role === 'admin';
  if (resource.kind === 'org') {
    if (action === 'read') return 'allow';
    if (!orgAdmin) return 'deny';
    return claims.principal === 'agent' && action === 'widen' ? 'proposal' : 'allow';
  }
  const role = Object.hasOwn(claims.project_roles, resource.projectId)
    ? claims.project_roles[resource.projectId] : undefined;
  if (!role) return 'deny';
  if (action === 'read') return 'allow';
  if (action === 'operate') return role === 'admin' ? 'allow' : 'deny';
  if (action === 'edit') return role === 'editor' || role === 'admin' ? 'allow' : 'deny';
  if (action === 'widen' && claims.principal === 'agent' && (role === 'editor' || role === 'admin')) return 'proposal';
  return role === 'admin' && action !== 'secret' ? 'allow' : 'deny';
}
