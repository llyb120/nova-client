import { createSignal, For, onMount, Show } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { showToast } from "../store";

/** 密码本条目（后端永远不返回密码）。 */
type Item = { id: string; name: string; condition: string; username: string; share: boolean; temp: boolean; usedAt: number; usedFor: string };
type Draft = { id?: string; condition: string; username: string; password: string; share: boolean };
const EMPTY: Draft = { condition: "", username: "", password: "", share: false };

/** 数字员工的密码本：每条密码要写明使用条件；员工需要账号密码时先查这里，远程员工要时经加密传输。 */
export default function VaultPop() {
  const [items, setItems] = createSignal<Item[]>([]);
  const [draft, setDraft] = createSignal<Draft | null>(null);
  const refresh = () => invoke<Item[]>("vault_list").then(setItems, (e) => showToast(String(e)));
  onMount(() => void refresh());
  const patch = (p: Partial<Draft>) => setDraft((d) => d && { ...d, ...p });
  const save = async () => {
    const d = draft();
    if (!d) return;
    try {
      // 后端仍要一个名称（通知、加密传输里用）：取账号，没有账号就取使用条件开头。
      await invoke("vault_save", { entry: { ...d, name: (d.username.trim() || d.condition.trim()).slice(0, 30) } });
      setDraft(null);
      void refresh();
    } catch (e) { showToast(String(e)); }
  };
  const remove = (i: Item) => {
    if (!confirm(`删除密码「${i.name}」？`)) return;
    invoke("vault_delete", { id: i.id }).then(() => void refresh(), (e) => showToast(String(e)));
  };
  return (
    <details class="employee-pop employee-memory employee-vault" onToggle={(ev) => { if (ev.currentTarget.open) void refresh(); else setDraft(null); }}>
      <summary>密码本 <span class="employee-count">{items().length}</span></summary>
      <div>
        <Show when={draft()} fallback={
          <>
            <p class="employee-hint">员工需要账号密码时先查这里，条件符合才取用。密码加密保存，只在输入时使用；发给远程员工时加密传输，不暴露明文。</p>
            <Show when={items().length} fallback={<p class="employee-empty employee-empty-inline">还没有密码。</p>}>
              <ul class="vault-list">
                <For each={items()}>{(i) => (
                  <li class="vault-item">
                    <div class="vault-item-head">
                      <strong>{i.temp ? i.name : i.username || i.name}</strong>
                      <Show when={i.temp}><span class="employee-tag">临时</span></Show>
                      <span class="employee-tag">{i.share ? "可加密发给远程员工" : "仅本机"}</span>
                    </div>
                    <small>使用条件：{i.condition}</small>
                    <Show when={i.usedAt}><small>最近取用：{new Date(i.usedAt).toLocaleString()}{i.usedFor ? ` · ${i.usedFor}` : ""}</small></Show>
                    <div class="vault-item-actions">
                      <Show when={!i.temp}><button type="button" onClick={() => setDraft({ id: i.id, condition: i.condition, username: i.username, password: "", share: i.share })}>编辑</button></Show>
                      <button type="button" class="vault-danger" onClick={() => remove(i)}>删除</button>
                    </div>
                  </li>
                )}</For>
              </ul>
            </Show>
            <button type="button" class="vault-add" onClick={() => setDraft({ ...EMPTY })}>+ 新增密码</button>
          </>
        }>
          {(d) => (
            <form class="vault-form" onSubmit={(ev) => { ev.preventDefault(); void save(); }}>
              <h3>{d().id ? "编辑密码" : "新增密码"}</h3>
              <label class="employee-field">使用条件
                <textarea rows={2} value={d().condition} maxLength={500} required placeholder="什么情况下员工才可以用，如：登录 TAPD 时" onInput={(ev) => patch({ condition: ev.currentTarget.value })} /></label>
              <div class="vault-pair">
                <label class="employee-field">账号
                  <input value={d().username} maxLength={200} autocomplete="off" onInput={(ev) => patch({ username: ev.currentTarget.value })} /></label>
                <label class="employee-field">密码{d().id ? "（留空不改）" : ""}
                  <input type="password" value={d().password} maxLength={1000} required={!d().id} autocomplete="new-password" onInput={(ev) => patch({ password: ev.currentTarget.value })} /></label>
              </div>
              <label class="employee-field employee-public">
                <input type="checkbox" checked={d().share} onChange={(ev) => patch({ share: ev.currentTarget.checked })} /> 允许加密发给远程员工
              </label>
              <div class="vault-actions">
                <button type="button" onClick={() => setDraft(null)}>取消</button>
                <button type="submit" class="vault-primary">保存</button>
              </div>
            </form>
          )}
        </Show>
      </div>
    </details>
  );
}