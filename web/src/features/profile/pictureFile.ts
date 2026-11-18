// A chosen image file as an upload the server takes (2026-11-17): cropped to
// a centred square, scaled to at most 512×512, PNG kept as PNG and anything
// else as JPEG, and under 1 MiB.
export const MAX_SIDE = 512;
export const MAX_BYTES = 1 << 20;

/** The square to crop from a `width`×`height` image, and the side to draw it at. */
export function squareCrop(width: number, height: number) {
  const side = Math.min(width, height);
  return {
    sx: Math.floor((width - side) / 2),
    sy: Math.floor((height - side) / 2),
    side,
    out: Math.min(side, MAX_SIDE),
  };
}

function toBlob(canvas: HTMLCanvasElement, type: string, quality?: number): Promise<Blob> {
  return new Promise((resolve, reject) =>
    canvas.toBlob((blob) => (blob ? resolve(blob) : reject(new Error('image'))), type, quality),
  );
}

async function base64(blob: Blob): Promise<string> {
  const bytes = new Uint8Array(await blob.arrayBuffer());
  let binary = '';
  for (let i = 0; i < bytes.length; i += 0x8000) {
    binary += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  }
  return btoa(binary);
}

/** The base64 upload for `file`, or an error message for the person. */
export async function pictureUpload(file: File): Promise<string> {
  if (!file.type.startsWith('image/')) throw new Error('Choose a PNG or JPEG image.');
  const bitmap = await createImageBitmap(file).catch(() => {
    throw new Error('That image could not be read. Choose a PNG or JPEG.');
  });
  const { sx, sy, side, out } = squareCrop(bitmap.width, bitmap.height);
  const canvas = document.createElement('canvas');
  canvas.width = out;
  canvas.height = out;
  const context = canvas.getContext('2d');
  if (!context) throw new Error('This browser cannot prepare the image.');
  context.drawImage(bitmap, sx, sy, side, side, 0, 0, out, out);
  bitmap.close();
  let blob = await toBlob(canvas, file.type === 'image/png' ? 'image/png' : 'image/jpeg', 0.9);
  // A large PNG photo goes as JPEG; then lower the quality until it fits.
  for (const quality of [0.9, 0.8, 0.65, 0.5]) {
    if (blob.size <= MAX_BYTES) break;
    blob = await toBlob(canvas, 'image/jpeg', quality);
  }
  if (blob.size > MAX_BYTES) throw new Error('That image is too large. Choose a smaller one.');
  return base64(blob);
}
