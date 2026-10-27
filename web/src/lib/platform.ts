// Works out which download suits the visitor's computer. Phones and tablets,
// and anything unrecognised, get `undefined`, and pages fall back to Windows.
import type { PlatformId } from '../data/platforms';

interface UAData {
  platform?: string;
  mobile?: boolean;
  getHighEntropyValues?: (
    hints: string[],
  ) => Promise<{ architecture?: string }>;
}

export async function detectPlatform(): Promise<PlatformId | undefined> {
  const data = (navigator as Navigator & { userAgentData?: UAData })
    .userAgentData;
  const ua = navigator.userAgent.toLowerCase();
  const platform = (data?.platform ?? navigator.platform ?? '').toLowerCase();

  if (data?.mobile || /android|iphone|ipad|ipod|mobile/.test(ua)) return;
  if (platform.includes('mac') || ua.includes('mac os x')) return 'macos';
  if (platform.includes('linux') || /linux|cros/.test(ua)) return 'linux';
  if (!platform.includes('win') && !ua.includes('windows')) return;

  // Browsers on Arm PCs mostly report x64; ask for the real architecture.
  try {
    const { architecture } =
      (await data?.getHighEntropyValues?.(['architecture'])) ?? {};
    if (architecture === 'arm') return 'windows-arm';
  } catch {}
  return /arm|aarch64/.test(ua) ? 'windows-arm' : 'windows';
}
