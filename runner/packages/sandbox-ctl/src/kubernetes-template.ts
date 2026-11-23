import type { GrantClaims } from '@runbook/runner-shared/contracts/core';
import type { SandboxSize } from '@runbook/runner-shared/contracts/internal';
import { NODE, SIZES, TRUSTED_HELPER } from './template.ts';

export const SANDBOX_CONTAINER = 'sandbox';
export const SANDBOX_LABEL = 'runbook.dev/sandbox';
export const DIGEST_IMAGE = /^[a-z0-9][a-z0-9./:_-]*@sha256:[a-f0-9]{64}$/;
export const KUBE_HELPER = [NODE, '/usr/local/lib/runbook-sandbox.mjs'];

export function sandboxAnnotations(id: string, claims: Pick<GrantClaims, 'sub' | 'claim_id' | 'generation'>) {
  return {
    'runbook.dev/sandbox-id': id,
    'runbook.dev/step-run': claims.sub,
    'runbook.dev/claim-id': claims.claim_id,
    'runbook.dev/generation': String(claims.generation),
  };
}

export interface KubernetesSandboxOptions {
  namePrefix?: string;
  serviceAccountName?: string;
  labels?: Record<string, string>;
  trustedImage?: string;
}

export function podTemplate(name: string, id: string, claims: Parameters<typeof sandboxAnnotations>[1],
  image: string, size: SandboxSize, timeoutMs: number, options: KubernetesSandboxOptions = {}) {
  if (!DIGEST_IMAGE.test(image)) throw new Error('RUNNER_SANDBOX_IMAGE must be pinned by digest');
  if (options.trustedImage && !DIGEST_IMAGE.test(options.trustedImage)) throw new Error('trusted sandbox image must be pinned by digest');
  const { trustedImage } = options;
  const custom = trustedImage !== undefined && image !== trustedImage;
  const securityContext = {
    readOnlyRootFilesystem: true, privileged: false, allowPrivilegeEscalation: false,
    capabilities: { drop: ['ALL'] },
  };
  const { vcpu, gib } = SIZES[size];
  // Use the Autopilot general-purpose defaults as the smallest profile,
  // including the footprint of the gVisor sandbox.
  const requests = { cpu: `${vcpu * 1000}m`, memory: `${Math.max(2, gib)}Gi`, 'ephemeral-storage': '1Gi' };
  const volumes = [
    { name: 'workspace', path: '/workspace', size: '512Mi' },
    { name: 'tmp', path: '/tmp', size: '256Mi' },
    { name: 'results', path: '/runbook-results', size: '256Mi' },
  ];
  const trusted = { name: 'trusted', emptyDir: { sizeLimit: '32Mi' } };
  const helperRequests = { cpu: '500m', memory: '2Gi', 'ephemeral-storage': '1Gi' };
  return {
    apiVersion: 'v1',
    kind: 'Pod',
    metadata: {
      name, labels: { ...options.labels, [SANDBOX_LABEL]: 'true' }, annotations: sandboxAnnotations(id, claims),
    },
    spec: {
      runtimeClassName: 'gvisor',
      serviceAccountName: options.serviceAccountName ?? 'sandbox',
      automountServiceAccountToken: false, enableServiceLinks: false,
      restartPolicy: 'Never', activeDeadlineSeconds: Math.ceil(timeoutMs / 1000),
      terminationGracePeriodSeconds: 1,
      securityContext: { runAsNonRoot: true, runAsUser: 10001, runAsGroup: 10001, fsGroup: 10001 },
      ...(custom && { initContainers: [{
        name: 'trusted-helper', image: trustedImage, imagePullPolicy: 'IfNotPresent',
        command: ['/bin/cp', TRUSTED_HELPER, '/copy/helper'],
        securityContext, resources: { requests: helperRequests, limits: helperRequests },
        volumeMounts: [{ name: 'trusted', mountPath: '/copy' }],
      }] }),
      containers: [{
        name: SANDBOX_CONTAINER, image, imagePullPolicy: 'IfNotPresent',
        command: custom ? [TRUSTED_HELPER, 'idle'] : ['/usr/bin/sleep', 'infinity'], workingDir: '/workspace',
        securityContext,
        resources: { requests, limits: requests },
        volumeMounts: [
          ...volumes.map(({ name, path }) => ({ name, mountPath: path })),
          ...(custom ? [{ name: 'trusted', mountPath: '/runbook-trusted', readOnly: true }] : []),
        ],
      }],
      volumes: [
        ...volumes.map(({ name, size }) => ({ name, emptyDir: { sizeLimit: size } })),
        ...(custom ? [trusted] : []),
      ],
    },
  };
}
