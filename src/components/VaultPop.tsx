import { createSignal, For, onMount, Show } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { showToast } from "../store";

/** 密码本条目（后端永远不返回密码）。 */
type Item = { id: string; name: string; condition: string; username: string; share: boolean; temp: boolean; usedAt: number; usedFor: string };
type Draft = { id?: string; name: string; condition: string; username: string; password: string; share: boolean };
const EMPTY: Draft = { name: "", condition: "", username: "", password: "", share: false };

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
      await invoke("vault_save", { entry: d });
      setDraft(null);
      void refresh();
    } catch (e) { showToast(String(e)); }
  };
  const remove = (i: Item) => {
    if (!confirm(`删除密码「${i.name}」？`)) return;
    invoke("vault_delete", { id: i.id }).then(() => void refresh(), (e) => showToast(String(e)));
  };
  return (
    <details class="employee-pop employee-memory" onToggle={(ev) => { if (ev.currentTarget.open) void refresh(); else setDraft(null); }}>
      <summary>密码本 <span class="employee-count">{items().length}</span></summary>
      <div>
        <p class="employee-hint">员工需要账号密码时先查这里，条件符合才会取用；密码加密保存，员工只在输入时用，远程员工要用时加密传输、不暴露明文。</p>
        <Show when={draft()} fallback={
          <>
            <Show when={items().length} fallback={<p class="employee-empty employee-empty-inline">还没有密码。</p>}>
              <ul class="employee-list">
                <For each={items()}>{(i) => (
                  <li>
                    <div class="employee-main">
                      <div class="employee-item-title">{i.name}{i.temp ? "（临时）" : ""}</div>
                      <small>{i.username ? `账号 ${i.username} · ` : ""}{i.share ? "允许加密发给远程员工" : "仅本机使用"}</small>
                      <small>使用条件：{i.condition}</small>
                      <Show when={i.usedAt}><small>最近取用：{new Date(i.usedAt).toLocaleString()}{i.usedFor ? ` · ${i.usedFor}` : ""}</small></Show>
                    </div>
                    <Show when={!i.temp}><button type="button" onClick={() => setDraft({ id: i.id, name: i.name, condition: i.condition, username: i.username, password: "", share: i.share })}>编辑</button></Show>
                    <button type="button" onClick={() => remove(i)}>删除</button>
                  </li>
                )}</For>
              </ul>
            </Show>
            <p><button type="button" onClick={() => setDraft({ ...EMPTY })}>+ 新增密码</button></p>
          </>
        }>
          {(d) => (
            <form onSubmit={(ev) => { ev.preventDefault(); void save(); }}>
              <label class="employee-field">名称（这是什么、用在哪）
                <input value={d().name} maxLength={100} required onInput={(ev) => patch({ name: ev.currentTarget.value })} /></label>
              <label class="employee-field">使用条件（什么情况下员工才可以用）
                <textarea rows={2} value={d().condition} maxLength={500} required onInput={(ev) => patch({ condition: ev.currentTarget.value })} /></label>
              <label class="employee-field">账号
                <input value={d().username} maxLength={200} autocomplete="off" onInput={(ev) => patch({ username: ev.currentTarget.value })} /></label>
              <label class="employee-field">密码{d().id ? "（留空不修改）" : ""}
                <input type="password" value={d().password} maxLength={1000} required={!d().id} autocomplete="new-password" onInput={(ev) => patch({ password: ev.currentTarget.value })} /></label>
              <label class="employee-field employee-public">
                <input type="checkbox" role="switch" checked={d().share} onChange={(ev) => patch({ share: ev.currentTarget.checked })} /> 允许加密发给远程员工
              </label>
              <p class="employee-edit-actions">
                <button type="submit" class="employee-primary">保存</button>{" "}
                <button type="button" onClick={() => setDraft(null)}>取消</button>
              </p>
            </form>
          )}
        </Show>
      </div>
    </details>
  );
}
