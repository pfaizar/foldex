function publicUrl(value: string | undefined): string | undefined {
  if (!value?.trim()) return undefined;

  const url = new URL(value);
  if (url.protocol !== 'https:') {
    throw new Error('Public repository and download URLs must use HTTPS.');
  }
  return url.href;
}

const repository = publicUrl(
  import.meta.env.PUBLIC_REPOSITORY_URL ?? 'https://github.com/pfaizar/foldex',
);

// Update with each release. Asset names follow scripts/package_installer.py.
const version = '0.1.5';
const assets = repository
  ? `${repository.replace(/\/$/, '')}/releases/download/v${version}`
  : undefined;

export const site = {
  name: 'Foldex',
  title: 'Foldex — Files, without the friction.',
  description:
    'A native Windows file explorer built in Rust. Browse in tabs and split panes, find files quickly, and keep your hands on the keyboard. Free and open source.',
  repository,
  download: publicUrl(
    import.meta.env.PUBLIC_DOWNLOAD_URL ??
      'https://github.com/pfaizar/foldex/releases',
  ),
  version,
  installer: assets
    ? {
        href: `${assets}/Foldex-${version}-windows-x64-setup.exe`,
        size: '9.8 MB',
      }
    : undefined,
  portable: assets
    ? {
        href: `${assets}/Foldex-${version}-windows-x64-portable.zip`,
        size: '11.4 MB',
      }
    : undefined,
  license: '/LICENSE.txt',
} as const;

export const navigation = [
  { label: 'Home', href: '/' },
  { label: 'Changelog', href: '/changelog/' },
  { label: 'Documentation', href: '/docs/' },
] as const;
