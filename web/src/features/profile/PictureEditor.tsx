// A large avatar with Upload and Remove (2026-11-17): the image is cropped
// square and scaled to 512×512 in the browser before it is sent.
import { ImageUp, Trash2 } from 'lucide-react';
import { useId, useRef, useState } from 'react';
import { Button } from '../../ui/primitives/button';
import { PictureAvatar, type PictureAvatarProps } from '../../ui/status/PictureAvatar';
import { pictureUpload } from './pictureFile';

export function PictureEditor({
  of,
  label,
  save,
  busy,
  disabled = false,
}: {
  of: PictureAvatarProps['of'];
  label: string;
  /** Store the base64 image, or remove it with `null`. */
  save(data: string | null): Promise<unknown>;
  busy: boolean;
  disabled?: boolean;
}) {
  const input = useRef<HTMLInputElement>(null);
  const errorId = useId();
  const [error, setError] = useState<string | null>(null);

  async function choose(file: File | undefined) {
    if (!file) return;
    setError(null);
    try {
      await save(await pictureUpload(file));
    } catch (e) {
      setError(e instanceof Error && e.message ? e.message : 'The image could not be saved.');
    } finally {
      if (input.current) input.current.value = '';
    }
  }

  return (
    <div className="flex flex-wrap items-center gap-4">
      <PictureAvatar of={of} label={label} className="size-16 text-lg" />
      <div className="grid gap-1.5">
        <div className="flex flex-wrap gap-2">
          <Button
            type="button"
            variant="secondary"
            size="sm"
            disabled={disabled || busy}
            onClick={() => input.current?.click()}
          >
            <ImageUp className="size-3.5" aria-hidden="true" />
            Upload image
          </Button>
          <Button
            type="button"
            variant="ghost"
            size="sm"
            disabled={disabled || busy}
            onClick={() => {
              setError(null);
              void save(null).catch(() => setError('The image could not be removed.'));
            }}
          >
            <Trash2 className="size-3.5" aria-hidden="true" />
            Remove
          </Button>
        </div>
        <p className="text-xs text-fg-muted">PNG or JPEG. It is cropped square to 512×512.</p>
        {error ? (
          <p id={errorId} role="alert" className="text-xs text-danger-text">
            {error}
          </p>
        ) : null}
        <input
          ref={input}
          type="file"
          accept="image/png,image/jpeg"
          className="sr-only"
          tabIndex={-1}
          aria-hidden="true"
          onChange={(e) => void choose(e.target.files?.[0])}
        />
      </div>
    </div>
  );
}
