import type { CloudSyncSettings } from './desktop-api'

export function cloudSyncProfileSnapshot(value: CloudSyncSettings) {
  const common = {
    backend: value.backend,
    enabled: value.enabled,
    encryption_enabled: value.encryption_enabled,
  }
  if (value.backend === 'webdav') {
    return {
      ...common,
      base_url: value.base_url,
      root_path: value.root_path,
      username: value.username,
    }
  }
  return {
    ...common,
    s3: { ...value.s3 },
  }
}

export function cloudSyncProfilesEqual(
  left: CloudSyncSettings,
  right: CloudSyncSettings,
): boolean {
  return JSON.stringify(cloudSyncProfileSnapshot(left))
    === JSON.stringify(cloudSyncProfileSnapshot(right))
}
