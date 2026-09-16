import { createEffect, onCleanup, onMount } from 'solid-js';
import { createUniver, LocaleType } from '@univerjs/presets';
import { UniverSheetsCorePreset } from '@univerjs/preset-sheets-core';
import zhCN from '@univerjs/preset-sheets-core/locales/zh-CN';
import '@univerjs/preset-sheets-core/lib/index.css';
import type { IWorkbookData } from '@univerjs/core';

export type SheetEditor = { flush: () => Promise<void> };
export default function WorkspaceSheet(props: { text: string; readOnly: boolean; onReady?: (text: string) => void; onChange: (text: string) => void; onEditor: (editor?: SheetEditor) => void }) {
  let host!: HTMLDivElement;
  onMount(() => {
    const { univer, univerAPI } = createUniver({
      locale: LocaleType.ZH_CN, locales: { [LocaleType.ZH_CN]: zhCN },
      presets: [UniverSheetsCorePreset({ container: host, header: true, toolbar: true, ribbonType: 'simple', disableAutoFocus: true })],
    });
    const workbook = univerAPI.createWorkbook(JSON.parse(props.text) as IWorkbookData);
    let last = JSON.stringify(workbook.save());
    props.onReady?.(last);
    const snapshot = () => {
      const text = JSON.stringify(workbook.save());
      if (text !== last) { last = text; props.onChange(text); }
    };
    const listener = univerAPI.onCommandExecuted(command => { if (command.type === 2) snapshot(); });
    props.onEditor({ flush: async () => { await workbook.endEditingAsync(true); snapshot(); } });
    createEffect(() => { host.inert = props.readOnly; });
    onCleanup(() => { props.onEditor(undefined); listener.dispose(); univer.dispose(); });
  });
  return <div class="workspace-sheet-host" ref={host} aria-label="Excel 表格编辑器" onKeyDown={e => {
    // 避免表格编辑快捷键触发外层会话快捷键；保存继续交给文件面板。
    if (!((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 's')) e.stopPropagation();
  }} />;
}
