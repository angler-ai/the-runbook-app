import { api, type Schemas } from '../../../api/client';
import { actionId, runAction } from '../actions';

export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return '0 B';
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

export function resolveMimeType(file: File): string {
  if (file.type) return file.type.toLowerCase();
  const ext = file.name.split('.').pop()?.toLowerCase();
  switch (ext) {
    case 'png':
      return 'image/png';
    case 'jpg':
    case 'jpeg':
      return 'image/jpeg';
    case 'webp':
      return 'image/webp';
    case 'gif':
      return 'image/gif';
    case 'heic':
    case 'heif':
      return 'image/heic';
    case 'svg':
      return 'image/svg+xml';
    case 'pdf':
      return 'application/pdf';
    case 'json':
      return 'application/json';
    case 'txt':
    case 'log':
      return 'text/plain';
    case 'md':
      return 'text/markdown';
    case 'csv':
      return 'text/csv';
    default:
      return 'application/octet-stream';
  }
}

export function isImageMime(mime: string): boolean {
  return mime.startsWith('image/');
}

const DEFAULT_ACCEPTED_IMAGES = ['image/png', 'image/jpeg', 'image/webp', 'image/gif'];

export function validateEvidenceFile(
  file: File,
  limits?: Schemas['UploadLimits'] | null,
  maxRequestBytes?: number | null,
): { ok: true; mime: string } | { ok: false; error: string } {
  const mime = resolveMimeType(file);

  // SVG images are never accepted as evidence (D51)
  if (mime === 'image/svg+xml' || file.name.toLowerCase().endsWith('.svg')) {
    return { ok: false, error: 'SVG images are not supported as evidence.' };
  }

  // HEIC is refused for now (D51)
  if (
    mime === 'image/heic' ||
    file.name.toLowerCase().endsWith('.heic') ||
    file.name.toLowerCase().endsWith('.heif')
  ) {
    return { ok: false, error: 'HEIC images are not supported.' };
  }

  const isImage = isImageMime(mime);

  if (isImage) {
    const allowedImageTypes =
      limits?.image_types && limits.image_types.length > 0
        ? limits.image_types
        : DEFAULT_ACCEPTED_IMAGES;

    if (!allowedImageTypes.includes(mime)) {
      return {
        ok: false,
        error: `Image format "${mime}" is not supported (accepted: ${allowedImageTypes.join(', ')}).`,
      };
    }

    if (limits?.image_bytes && file.size > limits.image_bytes) {
      return {
        ok: false,
        error: `Image size (${formatBytes(file.size)}) exceeds maximum allowed size of ${formatBytes(limits.image_bytes)}.`,
      };
    }
  } else {
    // Non-image check: don't restrict document types client-side; let the server decide.
    if (limits?.file_bytes && file.size > limits.file_bytes) {
      return {
        ok: false,
        error: `File size (${formatBytes(file.size)}) exceeds maximum allowed size of ${formatBytes(limits.file_bytes)}.`,
      };
    }
  }

  if (maxRequestBytes && file.size > maxRequestBytes) {
    return {
      ok: false,
      error: `File size exceeds server request limit of ${formatBytes(maxRequestBytes)}.`,
    };
  }

  return { ok: true, mime };
}

export interface UploadEvidenceOptions {
  stepRunId: string;
  file: File;
  mime: string;
  onProgress?: (loaded: number, total: number, percent: number) => void;
  signal?: AbortSignal;
}

export async function uploadEvidenceFile({
  stepRunId,
  file,
  mime,
  onProgress,
  signal,
}: UploadEvidenceOptions): Promise<Schemas['EvidenceView']> {
  if (signal?.aborted) {
    throw new DOMException('Upload aborted', 'AbortError');
  }

  // 1. Request upload slot
  const slotKey = actionId('evidence-slot', {
    stepRunId,
    name: file.name,
    size: file.size,
    lastModified: file.lastModified,
  });

  const slot = await runAction(slotKey, async (key) => {
    const { data } = await api.POST('/v1/step-runs/{sr}/evidence', {
      params: { path: { sr: stepRunId }, header: { 'Idempotency-Key': key } },
      body: { name: file.name, type: mime, size: file.size },
      signal,
    });
    return data as Schemas['EvidenceSlotView'];
  });

  if (signal?.aborted) {
    throw new DOMException('Upload aborted', 'AbortError');
  }

  // 2. Direct upload PUT to signed URL with progress via XMLHttpRequest
  await new Promise<void>((resolve, reject) => {
    if (signal?.aborted) {
      reject(new DOMException('Upload aborted', 'AbortError'));
      return;
    }

    const xhr = new XMLHttpRequest();
    xhr.open('PUT', slot.upload_url);

    const headers: Record<string, string> = { 'Content-Type': mime };
    if (slot.headers) {
      for (const [k, v] of Object.entries(slot.headers)) {
        headers[k] = String(v);
      }
    }
    for (const [k, v] of Object.entries(headers)) {
      xhr.setRequestHeader(k, v);
    }

    if (xhr.upload && onProgress) {
      xhr.upload.onprogress = (event: ProgressEvent) => {
        const loaded = event.loaded;
        const total = event.lengthComputable && event.total > 0 ? event.total : file.size;
        const percent = total > 0 ? Math.min(100, Math.round((loaded / total) * 100)) : 0;
        onProgress(loaded, total, percent);
      };
    }

    xhr.onload = () => {
      if (xhr.status >= 200 && xhr.status < 300) {
        if (onProgress) {
          onProgress(file.size, file.size, 100);
        }
        resolve();
      } else {
        reject(new Error(`Evidence upload failed with HTTP ${xhr.status}`));
      }
    };

    xhr.onerror = () => {
      reject(new Error('Evidence upload failed'));
    };

    xhr.onabort = () => {
      reject(new DOMException('Upload aborted', 'AbortError'));
    };

    if (signal) {
      if (signal.aborted) {
        xhr.abort();
        return;
      }
      signal.addEventListener('abort', () => xhr.abort(), { once: true });
    }

    xhr.send(file);
  });

  if (signal?.aborted) {
    throw new DOMException('Upload aborted', 'AbortError');
  }

  // 3. Finalize upload
  const finalizeKey = actionId('evidence-finalize', {
    stepRunId,
    evidenceId: slot.evidence_id,
  });

  const finalized = await runAction(finalizeKey, async (key) => {
    const { data } = await api.POST('/v1/step-runs/{sr}/evidence/{evd}/finalize', {
      params: {
        path: { sr: stepRunId, evd: slot.evidence_id },
        header: { 'Idempotency-Key': key },
      },
      signal,
    });
    return data as Schemas['EvidenceView'];
  });

  return finalized;
}
