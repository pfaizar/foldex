function publicUrl(value: string | undefined): string | undefined {
  if (!value?.trim()) return undefined;

  const url = new URL(value);
  if (url.protocol !== 'https:') {
    throw new Error('Public repository and download URLs must use HTTPS.');
  }
  return url.href;
}

export const site = {
  name: 'Foldex',
  title: 'Foldex — Files, without the friction.',
  description:
    'A native Windows file explorer built in Rust. Browse in tabs and split panes, find files quickly, and keep your hands on the keyboard. Free and open source.',
  repository: publicUrl(
    import.meta.env.PUBLIC_REPOSITORY_URL ??
      'https://github.com/pfaizar/foldex',
  ),
  download: publicUrl(
    import.meta.env.PUBLIC_DOWNLOAD_URL ??
      'https://github.com/pfaizar/foldex/releases',
  ),
  license: '/LICENSE.txt',
} as const;

export const navigation = [
  { label: 'Features', href: '#features' },
  { label: 'Keyboard', href: '#keyboard' },
  { label: 'Open source', href: '#open-source' },
] as const;
