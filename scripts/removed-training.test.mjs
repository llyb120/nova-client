import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";

const root = new URL("../", import.meta.url);
const read = (path) => readFileSync(new URL(path, root), "utf8");
for (const path of ["src-tauri/src/experience.rs", "src/components/TrainingGroundView.tsx", "src/training-ground.css"]) {
  assert.equal(existsSync(new URL(path, root)), false, path);
}
for (const path of ["src/ipc.ts", "src/store.ts", "src/components/HomeView.tsx", "src/components/SettingsModal.tsx", "src-tauri/src/lib.rs", "src-tauri/src/settings.rs", "src-tauri/src/lyra/tools.rs"]) {
  assert.doesNotMatch(read(path), /trainExperience|experience::|experience_training_enabled|experienceTrainingEnabled|NOVA_EXPERIENCE_TOOLS/, path);
}
assert.doesNotMatch(read("src/components/slashSuggestions.ts"), /name: "train"/);
assert.match(read("src-tauri/src/lyra/tools.rs"), /context::polaris\(&code_root, args\)/);
// Removing the feature must not expose historical local-only records to remote sync.
assert.match(read("src-tauri/src/remote.rs"), /!t\.experience_thread/);
assert.match(read("src-tauri/src/lib.rs"), /!thread\.experience_thread && !thread\.starred/);
console.log("Removed training entry points and legacy isolation: OK");
