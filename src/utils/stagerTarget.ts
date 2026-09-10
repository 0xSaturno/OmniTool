/**
 * Default in-project destination for "Send to Stager".
 *
 * Project assets live at `projects/<name>/<span>/<asset path>`, so a file
 * opened straight out of a project already carries its asset path. Falling
 * back to the bare filename dropped edited assets in the project root instead
 * of overwriting the original, leaving a stray copy the game never loads.
 */
export function deriveStagerTarget(sourceFile: string, assetPath?: string | null): string {
  if (assetPath) return `0/${assetPath}`;

  const norm = sourceFile.replace(/\\/g, "/");
  const match = norm.match(/\/projects\/[^/]+\/(\d+)\/(.+)$/i);
  if (match) return `${match[1]}/${match[2]}`;

  return `0/${norm.split("/").pop() ?? sourceFile}`;
}
