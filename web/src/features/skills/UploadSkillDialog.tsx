import { useCallback, useRef, useState, type DragEvent } from 'react';
import {
  AlertCircle,
  CheckCircle2,
  Copy,
  Check,
  FileCode,
  FolderUp,
  Loader2,
  Upload,
} from 'lucide-react';
import { useMeta } from '../../api/hooks';
import { Button } from '../../ui/primitives/button';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '../../ui/primitives/dialog';
import { Input } from '../../ui/primitives/input';
import { Label } from '../../ui/primitives/label';
import { MonoRef } from '../../ui/status/MonoRef';
import { useUploadSkill, type SkillVersionView } from './hooks';
import {
  DEFAULT_MAX_BUNDLE_BYTES,
  validateSkillFiles,
  type InputSkillFile,
  type SkillValidationResult,
} from './validation';

export interface UploadSkillDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  projectId: string | undefined;
  expectedName?: string;
  onSuccess?: (skill: SkillVersionView) => void;
}

function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return '0 B';
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

export function UploadSkillDialog({
  open,
  onOpenChange,
  projectId,
  expectedName,
  onSuccess,
}: UploadSkillDialogProps) {
  const meta = useMeta();
  const maxBytes = meta.data?.limits?.skill_bundle_bytes ?? DEFAULT_MAX_BUNDLE_BYTES;
  const uploadMutation = useUploadSkill(projectId);

  const folderInputRef = useRef<HTMLInputElement>(null);
  const filesInputRef = useRef<HTMLInputElement>(null);

  const [isDragging, setIsDragging] = useState(false);
  const [isValidating, setIsValidating] = useState(false);
  const [validationError, setValidationError] = useState<string | null>(null);
  const [validatedResult, setValidatedResult] = useState<SkillValidationResult | null>(null);
  const [reference, setReference] = useState('');
  const [uploadResult, setUploadResult] = useState<SkillVersionView | null>(null);
  const [copiedHash, setCopiedHash] = useState(false);

  const resetState = useCallback(() => {
    setValidationError(null);
    setValidatedResult(null);
    setReference('');
    setUploadResult(null);
    setCopiedHash(false);
    setIsValidating(false);
  }, []);

  const handleOpenChange = (nextOpen: boolean) => {
    if (!nextOpen) {
      resetState();
    }
    onOpenChange(nextOpen);
  };

  const processFiles = useCallback(
    async (inputFiles: InputSkillFile[]) => {
      setValidationError(null);
      setValidatedResult(null);
      setIsValidating(true);
      try {
        const result = await validateSkillFiles(inputFiles, {
          maxBytes,
          expectedName,
        });
        setValidatedResult(result);
      } catch (err) {
        setValidationError(err instanceof Error ? err.message : String(err));
      } finally {
        setIsValidating(false);
      }
    },
    [maxBytes, expectedName],
  );

  const handleFolderSelect = (e: React.ChangeEvent<HTMLInputElement>) => {
    const fileList = e.target.files;
    if (!fileList || fileList.length === 0) return;
    const inputFiles: InputSkillFile[] = Array.from(fileList).map((f) => ({
      file: f,
      relativePath: f.webkitRelativePath,
    }));
    void processFiles(inputFiles);
  };

  const handleFilesSelect = (e: React.ChangeEvent<HTMLInputElement>) => {
    const fileList = e.target.files;
    if (!fileList || fileList.length === 0) return;
    const inputFiles: InputSkillFile[] = Array.from(fileList).map((f) => ({
      file: f,
    }));
    void processFiles(inputFiles);
  };

  const handleDragOver = (e: DragEvent<HTMLDivElement>) => {
    e.preventDefault();
    e.stopPropagation();
    setIsDragging(true);
  };

  const handleDragLeave = (e: DragEvent<HTMLDivElement>) => {
    e.preventDefault();
    e.stopPropagation();
    setIsDragging(false);
  };

  const handleDrop = async (e: DragEvent<HTMLDivElement>) => {
    e.preventDefault();
    e.stopPropagation();
    setIsDragging(false);

    const items = e.dataTransfer.items;
    if (items && items.length > 0) {
      const inputFiles: InputSkillFile[] = [];

      const readEntry = async (entry: FileSystemEntry, currentPath: string): Promise<void> => {
        if (entry.isFile) {
          const fileEntry = entry as FileSystemFileEntry;
          const file = await new Promise<File>((resolve, reject) => {
            fileEntry.file(resolve, reject);
          });
          const relPath = currentPath ? `${currentPath}/${file.name}` : file.name;
          inputFiles.push({ file, relativePath: relPath });
        } else if (entry.isDirectory) {
          const dirEntry = entry as FileSystemDirectoryEntry;
          const dirReader = dirEntry.createReader();
          const nextPath = currentPath ? `${currentPath}/${entry.name}` : entry.name;
          const entries = await new Promise<FileSystemEntry[]>((resolve, reject) => {
            dirReader.readEntries(resolve, reject);
          });
          for (const child of entries) {
            await readEntry(child, nextPath);
          }
        }
      };

      try {
        const promises: Promise<void>[] = [];
        for (let i = 0; i < items.length; i++) {
          const item = items[i];
          if (!item) continue;
          const entry = item.webkitGetAsEntry ? item.webkitGetAsEntry() : null;
          if (entry) {
            promises.push(readEntry(entry, ''));
          } else {
            const f = item.getAsFile();
            if (f) inputFiles.push({ file: f });
          }
        }
        await Promise.all(promises);
        if (inputFiles.length > 0) {
          void processFiles(inputFiles);
          return;
        }
      } catch {
        // Fallback to dataTransfer.files
      }
    }

    if (e.dataTransfer.files && e.dataTransfer.files.length > 0) {
      const inputFiles: InputSkillFile[] = Array.from(e.dataTransfer.files).map((f) => ({
        file: f,
      }));
      void processFiles(inputFiles);
    }
  };

  const handleUpload = async () => {
    if (!validatedResult) return;
    try {
      const payload = {
        files: validatedResult.files.map((f) => ({
          path: f.path,
          content: f.content,
        })),
        reference: reference.trim() || null,
      };
      const res = await uploadMutation.mutateAsync(payload);
      setUploadResult(res);
      onSuccess?.(res);
    } catch (err) {
      setValidationError(err instanceof Error ? err.message : String(err));
    }
  };

  const handleCopyHash = () => {
    if (!uploadResult?.hash) return;
    void navigator.clipboard.writeText(uploadResult.hash);
    setCopiedHash(true);
    setTimeout(() => setCopiedHash(false), 2000);
  };

  const isNewVersion = Boolean(expectedName);
  const titleText = isNewVersion ? `New version for ${expectedName}` : 'New skill';

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{titleText}</DialogTitle>
          <DialogDescription>
            {isNewVersion
              ? `Upload updated SKILL.md and files for ${expectedName}.`
              : 'Upload a skill folder containing SKILL.md and supporting assets.'}
          </DialogDescription>
        </DialogHeader>

        {uploadResult ? (
          <div className="space-y-4 py-3">
            <div className="flex items-center gap-3 rounded-lg border border-verdict-pass-border bg-verdict-pass-bg p-4 text-verdict-pass-fg">
              <CheckCircle2 className="size-5 shrink-0 text-verdict-pass-fg" />
              <div className="min-w-0 flex-1">
                <p className="font-medium text-sm">Skill uploaded successfully</p>
                <p className="text-xs opacity-90">
                  Version created for <span className="font-semibold">{uploadResult.name}</span>
                </p>
              </div>
            </div>

            <div className="rounded-lg border border-border bg-surface-raised p-4 space-y-2">
              <span className="text-xs text-fg-muted font-medium">Resulting version hash</span>
              <div className="flex items-center justify-between gap-2">
                <MonoRef className="text-xs break-all select-all font-mono">
                  {uploadResult.hash}
                </MonoRef>
                <Button
                  size="sm"
                  variant="ghost"
                  onClick={handleCopyHash}
                  className="shrink-0 h-8 px-2"
                  aria-label="Copy version hash"
                >
                  {copiedHash ? (
                    <Check className="size-3.5 text-verdict-pass-fg" aria-hidden="true" />
                  ) : (
                    <Copy className="size-3.5" aria-hidden="true" />
                  )}
                </Button>
              </div>
            </div>

            <DialogFooter className="pt-2">
              <Button onClick={() => handleOpenChange(false)} className="w-full sm:w-auto">
                Done
              </Button>
            </DialogFooter>
          </div>
        ) : (
          <div className="space-y-4 py-2">
            {/* Hidden file inputs */}
            <input
              type="file"
              ref={folderInputRef}
              onChange={handleFolderSelect}
              className="hidden"
              // @ts-expect-error webkitdirectory is standard for folder picker
              webkitdirectory=""
              directory=""
              multiple
            />
            <input
              type="file"
              ref={filesInputRef}
              onChange={handleFilesSelect}
              className="hidden"
              multiple
            />

            {/* Dropzone */}
            {/* eslint-disable-next-line jsx-a11y/no-static-element-interactions */}
            <div
              onDragOver={handleDragOver}
              onDragLeave={handleDragLeave}
              onDrop={handleDrop}
              className={`flex flex-col items-center justify-center gap-3 rounded-lg border-2 border-dashed p-6 text-center transition-colors ${
                isDragging
                  ? 'border-accent bg-accent/5'
                  : 'border-border bg-surface hover:bg-surface-raised/50'
              }`}
            >
              <div className="size-10 rounded-full bg-surface-raised flex items-center justify-center text-fg-muted border border-border">
                <Upload className="size-5" aria-hidden="true" />
              </div>

              <div className="space-y-1">
                <p className="text-sm font-medium text-fg">
                  Drag and drop a skill folder or files here
                </p>
                <p className="text-xs text-fg-muted">
                  Must include <code className="font-mono text-fg">SKILL.md</code> with frontmatter.
                  Max bundle size: {formatBytes(maxBytes)}.
                </p>
              </div>

              <div className="flex items-center gap-2 pt-1">
                <Button
                  type="button"
                  variant="secondary"
                  size="sm"
                  onClick={() => folderInputRef.current?.click()}
                  className="gap-1.5"
                >
                  <FolderUp className="size-3.5" aria-hidden="true" />
                  <span>Choose folder</span>
                </Button>
                <Button
                  type="button"
                  variant="ghost"
                  size="sm"
                  onClick={() => filesInputRef.current?.click()}
                  className="gap-1.5"
                >
                  <FileCode className="size-3.5" aria-hidden="true" />
                  <span>Choose files</span>
                </Button>
              </div>
            </div>

            {isValidating ? (
              <div className="flex items-center gap-2 text-xs text-fg-muted py-1" role="status">
                <Loader2 className="size-3.5 animate-spin" aria-hidden="true" />
                <span>Validating skill files…</span>
              </div>
            ) : null}

            {validationError ? (
              <div
                role="alert"
                className="flex items-start gap-2.5 rounded-lg border border-danger/30 bg-danger/10 p-3 text-xs text-danger"
              >
                <AlertCircle className="size-4 shrink-0 mt-0.5" aria-hidden="true" />
                <div className="space-y-0.5">
                  <p className="font-medium">Validation failed</p>
                  <p className="break-words">{validationError}</p>
                </div>
              </div>
            ) : null}

            {validatedResult ? (
              <div className="rounded-lg border border-border bg-surface-raised p-3.5 space-y-2.5 text-xs">
                <div className="flex items-center justify-between border-b border-border pb-2">
                  <div className="flex items-center gap-1.5">
                    <span className="font-medium text-fg">{validatedResult.name}</span>
                    <span className="text-fg-muted font-mono">
                      ({validatedResult.files.length} files,{' '}
                      {formatBytes(validatedResult.totalBytes)})
                    </span>
                  </div>
                  <span className="rounded bg-verdict-pass-bg px-1.5 py-0.5 font-medium text-verdict-pass-fg border border-verdict-pass-border text-[10px]">
                    Valid bundle
                  </span>
                </div>

                <p className="text-fg-muted line-clamp-2">{validatedResult.description}</p>

                <div className="max-h-28 overflow-y-auto space-y-1 pt-1 font-mono text-[11px] text-fg-muted">
                  {validatedResult.files.map((file) => (
                    <div key={file.path} className="flex justify-between">
                      <span className="truncate pr-2">{file.path}</span>
                      <span className="shrink-0">{formatBytes(file.size)}</span>
                    </div>
                  ))}
                </div>
              </div>
            ) : null}

            <div className="grid gap-1.5 pt-1">
              <Label htmlFor="skill-reference">Git reference / provenance (optional)</Label>
              <Input
                id="skill-reference"
                value={reference}
                onChange={(e) => setReference(e.target.value)}
                placeholder="e.g. github.com/owner/repo@v1.0.0"
              />
            </div>

            <DialogFooter className="gap-2 pt-2">
              <Button
                type="button"
                variant="secondary"
                onClick={() => handleOpenChange(false)}
                disabled={uploadMutation.isPending}
              >
                Cancel
              </Button>
              <Button
                type="button"
                onClick={() => void handleUpload()}
                disabled={!validatedResult || uploadMutation.isPending || isValidating}
                className="gap-1.5"
              >
                {uploadMutation.isPending ? (
                  <>
                    <Loader2 className="size-3.5 animate-spin" aria-hidden="true" />
                    <span>Uploading…</span>
                  </>
                ) : (
                  <>
                    <Upload className="size-3.5" aria-hidden="true" />
                    <span>{isNewVersion ? 'Upload version' : 'Upload skill'}</span>
                  </>
                )}
              </Button>
            </DialogFooter>
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}
