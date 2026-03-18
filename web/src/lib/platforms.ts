export const PLATFORM_COLORS: Record<string, string> = {
  api: '#0891b2',
  telegram: '#2563eb',
  discord: '#7c3aed',
  slack: '#dc2626',
  whatsapp: '#16a34a',
  homeassistant: '#f59e0b',
}

export function getPlatformColor(platform: string): string {
  return PLATFORM_COLORS[platform.toLowerCase()] ?? '#525252'
}
