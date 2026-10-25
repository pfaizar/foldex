export function canIndex(site: URL | undefined): boolean {
  const isCloudflarePreview =
    import.meta.env.CF_PAGES === '1' &&
    import.meta.env.CF_PAGES_BRANCH !==
      (import.meta.env.PRODUCTION_BRANCH || 'main');

  return Boolean(site) && !isCloudflarePreview;
}
