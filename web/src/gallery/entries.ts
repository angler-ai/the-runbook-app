import { collectGalleryEntries, type GalleryModule } from './registry';

const modules = import.meta.glob<GalleryModule>('../**/*.gallery.tsx', { eager: true });

export const galleryEntries = collectGalleryEntries(modules);
