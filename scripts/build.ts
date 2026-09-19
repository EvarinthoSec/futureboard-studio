import { readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { spawnSync } from "node:child_process";

const workspaceRoot = join(import.meta.dir, "..");
const pluginCratesDir = join(
  workspaceRoot,
  "crates",
  "BuiltinAudioPlugins",
  "crates",
);
const requestedPlugins = new Set(
  process.argv.slice(2).filter((argument) => argument !== "--"),
);
const editorDirNames = ["editorui", "editor"];

type PluginEditor = {
  plugin: string;
  directory: string;
};

function discoverPluginEditors(): PluginEditor[] {
  return readdirSync(pluginCratesDir)
    .filter((plugin) => statSync(join(pluginCratesDir, plugin)).isDirectory())
    .filter(
      (plugin) =>
        requestedPlugins.size === 0 || requestedPlugins.has(plugin),
    )
    .flatMap((plugin) =>
      editorDirNames
        .map((directory) => ({
          plugin,
          directory: join(pluginCratesDir, plugin, directory),
        }))
        .filter(({ directory }) =>
          Bun.file(join(directory, "package.json")).size > 0
        ),
    )
    .sort((left, right) => left.plugin.localeCompare(right.plugin));
}

function runBun(
  editor: PluginEditor,
  args: string[],
  cwd = editor.directory,
): void {
  const command = spawnSync(process.execPath, args, {
    cwd,
    stdio: "inherit",
  });
  if (command.error) {
    throw command.error;
  }
  if (command.status !== 0) {
    process.exit(command.status ?? 1);
  }
}

const editors = discoverPluginEditors();
if (editors.length === 0) {
  console.log("[plugin-editors] no matching frontend bundles found");
  process.exit(0);
}

for (const editor of editors) {
  const relative = editor.directory.slice(workspaceRoot.length + 1);
  console.log(`[plugin-editors] installing ${editor.plugin}: ${relative}`);
  // Resolve the workspace lockfile from the root, but link only this editor's
  // dependency graph. Installing every workspace for each selected editor can
  // fail while linking unrelated workspace binaries on Windows.
  const workspaceFilter = `./${relative.replaceAll("\\", "/")}`;
  runBun(
    editor,
    ["install", "--frozen-lockfile", "--filter", workspaceFilter],
    workspaceRoot,
  );

  console.log(`[plugin-editors] building ${editor.plugin}: ${relative}`);
  runBun(editor, ["run", "build"]);
}
