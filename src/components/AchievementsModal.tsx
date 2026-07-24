import { createSignal, For, onMount, Show } from "solid-js";
import { markAchievementsSeen, refreshAchievements, state } from "../store";
import { AchievementBadge } from "./AchievementBadge";
import { IconTrophy, IconX } from "./icons";

export function AchievementsModal(props: { onClose: () => void }) {
  const [loading, setLoading] = createSignal(!state.achievementsLoaded);
  // 打开瞬间的未读快照：弹窗内 NEW 标记持续展示，侧栏角标则立即清除。
  // 若本次会话内重新拉取，新出现的未读 id 也会并进来，保持「这次看到的就是新的」。
  const [freshIds, setFreshIds] = createSignal(new Set(state.unseenAchievementIds));

  const settleFresh = () => {
    if (state.unseenAchievementIds.length > 0) {
      setFreshIds((prev) => new Set([...prev, ...state.unseenAchievementIds]));
      markAchievementsSeen();
    }
  };

  onMount(() => {
    markAchievementsSeen();
    if (!state.achievementsLoaded) {
      void refreshAchievements().then(() => {
        settleFresh();
        setLoading(false);
      });
    }
  });

  const hasRelayToken = () => !!state.settings?.relayToken?.trim();

  const reload = async () => {
    setLoading(true);
    await refreshAchievements(true);
    settleFresh();
    setLoading(false);
  };

  return (
    <div class="modal-backdrop" onClick={props.onClose}>
      <div class="modal achv-modal" onClick={(e) => e.stopPropagation()}>
        <button class="icon-btn achv-close" title="关闭" onClick={props.onClose}>
          <IconX size={16} />
        </button>

        <header class="achv-hero">
          <div class="achv-hero-medal">
            <IconTrophy size={30} />
          </div>
          <div class="achv-hero-text">
            <div class="achv-hero-count">
              <span class="achv-hero-num">{state.achievements.length}</span>
              <span class="achv-hero-unit">项成就已解锁</span>
            </div>
            <p class="achv-hero-sub">每一枚徽章，都是你和 Nova 一起走过的里程碑</p>
          </div>
          <Show when={hasRelayToken()}>
            <button type="button" class="link-btn achv-refresh" disabled={loading()} onClick={() => void reload()}>
              刷新
            </button>
          </Show>
        </header>

        <div class="achv-modal-body">
          <Show
            when={hasRelayToken()}
            fallback={
              <div class="achv-empty">
                <div class="achv-empty-icon"><IconTrophy size={34} /></div>
                <p class="achv-empty-title">成就殿堂尚未开启</p>
                <p class="achv-empty-desc">
                  成就由中转站按你的身份授予。前往「设置 → 团队」配置中转站 token，解锁属于你的徽章。
                </p>
              </div>
            }
          >
            <Show
              when={!loading()}
              fallback={
                <div class="achv-empty">
                  <span class="spinner" />
                  <p class="achv-empty-desc">正在从服务器读取成就…</p>
                </div>
              }
            >
              <Show
                when={!state.achievementsError}
                fallback={
                  <div class="achv-empty">
                    <p class="achv-empty-title">没能接上成就服务器</p>
                    <p class="achv-empty-desc achv-error">{state.achievementsError}</p>
                    <button type="button" class="btn secondary" onClick={() => void reload()}>
                      重试
                    </button>
                  </div>
                }
              >
                <Show
                  when={state.achievements.length > 0}
                  fallback={
                    <div class="achv-empty">
                      <div class="achv-empty-icon"><IconTrophy size={34} /></div>
                      <p class="achv-empty-title">第一枚徽章正在路上</p>
                      <p class="achv-empty-desc">继续和 Nova 一起完成任务，成就将由服务器按身份授予。</p>
                    </div>
                  }
                >
                  <div class="achv-grid">
                    <For each={state.achievements}>
                      {(a, i) => (
                        <AchievementBadge achievement={a} index={i()} isNew={freshIds().has(a.id)} />
                      )}
                    </For>
                  </div>
                </Show>
              </Show>
            </Show>
          </Show>
        </div>
      </div>
    </div>
  );
}
