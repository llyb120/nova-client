import ExcelJS from 'exceljs';
import type { IWorkbookData, IWorksheetData, ICellData, IStyleData, IColorStyle } from '@univerjs/core';

const borders = ['', 'thin', 'hair', 'dotted', 'dashed', 'dashDot', 'dashDotDot', 'double', 'medium', 'mediumDashed', 'mediumDashDot', 'mediumDashDotDot', 'slantDashDot', 'thick'] as const;
const horizontal = ['', 'left', 'center', 'right', 'justify', 'justify', 'distributed'] as const;
const vertical = ['', 'top', 'middle', 'bottom'] as const;
const rgb = (color?: Partial<ExcelJS.Color>) => color?.argb ? `#${color.argb.slice(-6)}` : undefined;
const argb = (color?: IColorStyle | null | void) => color?.rgb?.match(/^#[\da-f]{6}$/i) ? { argb: `FF${color.rgb.slice(1)}` } : undefined;

function importStyle(cell: ExcelJS.Cell): IStyleData {
  const { font = {}, alignment = {}, fill, border = {} } = cell;
  const style: IStyleData = {
    ff: font.name, fs: font.size, bl: font.bold ? 1 : 0, it: font.italic ? 1 : 0,
    ul: { s: font.underline ? 1 : 0 }, st: { s: font.strike ? 1 : 0 },
    cl: rgb(font.color) ? { rgb: rgb(font.color) } : undefined,
    bg: fill?.type === 'pattern' && fill.pattern === 'solid' && rgb(fill.fgColor) ? { rgb: rgb(fill.fgColor) } : undefined,
    ht: Math.max(0, horizontal.indexOf(alignment.horizontal as typeof horizontal[number])),
    vt: Math.max(0, vertical.indexOf(alignment.vertical as typeof vertical[number])),
    tb: alignment.wrapText ? 3 : 1,
    n: cell.numFmt ? { pattern: cell.numFmt } : undefined,
    bd: {},
  };
  for (const [key, edge] of [['t', 'top'], ['b', 'bottom'], ['l', 'left'], ['r', 'right']] as const) {
    const b = border[edge];
    if (b?.style) style.bd![key] = { s: Math.max(0, borders.indexOf(b.style)), cl: { rgb: rgb(b.color) ?? '#000000' } };
  }
  return style;
}

export async function importSpreadsheet(data: string, name: string): Promise<IWorkbookData> {
  const workbook = new ExcelJS.Workbook();
  // ponytail: 当前转换不支持数据校验；跳过解析，避免 ExcelJS 将整列规则展开为数百万个对象。
  // 支持校验时应按范围导入 Univer，而不是恢复 ExcelJS 的逐格展开。
  await workbook.xlsx.load(Uint8Array.from(atob(data), c => c.charCodeAt(0)) as unknown as ExcelJS.Buffer, { ignoreNodes: ['dataValidations'] });
  const sheets: IWorkbookData['sheets'] = {};
  let count = 0;
  for (const ws of workbook.worksheets) {
    const id = String(ws.id);
    const sheet: Partial<IWorksheetData> = {
      id, name: ws.name, hidden: ws.state === 'visible' ? 0 : 1,
      rowCount: Math.max(100, ws.rowCount + 20), columnCount: Math.max(26, ws.columnCount + 5),
      cellData: {}, rowData: {}, columnData: {}, mergeData: [],
    };
    ws.eachRow({ includeEmpty: false }, (row, r) => {
      sheet.rowData![r - 1] = { h: row.height ? row.height * 4 / 3 : undefined, hd: row.hidden ? 1 : 0 };
      row.eachCell({ includeEmpty: true }, (cell, c) => {
        // ponytail: 限制 10 万个单元格；更大工作簿需 worker 解析和增量快照。
        if (++count > 100_000) throw new Error('表格超过 10 万个单元格，请使用系统打开');
        if (cell.isMerged && cell.master.address !== cell.address) return;
        let value = cell.value;
        if (value instanceof Date) value = value.getTime() / 86400000 + 25569;
        else if (value && typeof value === 'object') {
          if ('formula' in value || 'sharedFormula' in value) value = cell.result ?? null;
          else if ('richText' in value) value = value.richText.map(run => run.text).join('');
          else if ('text' in value) value = value.text;
          else if ('error' in value) value = value.error;
        }
        if (value instanceof Date) value = value.getTime() / 86400000 + 25569;
        const v = typeof value === 'string' || typeof value === 'number' || typeof value === 'boolean' ? value : null;
        const data: ICellData = { v, t: typeof v === 'number' ? 2 : typeof v === 'boolean' ? 3 : 1, s: importStyle(cell) };
        if (cell.formula) data.f = `=${cell.formula}`;
        (sheet.cellData![r - 1] ??= {})[c - 1] = data;
      });
    });
    (ws.columns ?? []).forEach((col, c) => { sheet.columnData![c] = { w: col.width ? col.width * 7 + 5 : undefined, hd: col.hidden ? 1 : 0 }; });
    for (const range of ws.model.merges ?? []) {
      const [start, end = start] = range.split(':');
      const first = ws.getCell(start), last = ws.getCell(end);
      sheet.mergeData!.push({ startRow: Number(first.row) - 1, endRow: Number(last.row) - 1, startColumn: Number(first.col) - 1, endColumn: Number(last.col) - 1 });
    }
    const frozen = ws.views?.find(view => view.state === 'frozen');
    if (frozen?.state === 'frozen') sheet.freeze = { xSplit: frozen.xSplit ?? 0, ySplit: frozen.ySplit ?? 0, startRow: frozen.ySplit ?? 0, startColumn: frozen.xSplit ?? 0 };
    sheets[id] = sheet;
  }
  if (!Object.keys(sheets).length) throw new Error('工作簿没有可显示的工作表');
  return { id: crypto.randomUUID(), name, appVersion: '0.25.1', locale: 'zhCN' as IWorkbookData['locale'], styles: {}, sheetOrder: Object.keys(sheets), sheets };
}

export async function exportSpreadsheet(snapshot: IWorkbookData): Promise<string> {
  const workbook = new ExcelJS.Workbook();
  workbook.calcProperties.fullCalcOnLoad = true;
  for (const id of snapshot.sheetOrder) {
    const sheet = snapshot.sheets[id];
    const ws = workbook.addWorksheet(sheet.name || 'Sheet', { state: sheet.hidden ? 'hidden' : 'visible' });
    for (const [r, row] of Object.entries(sheet.cellData ?? {})) {
      for (const [c, data] of Object.entries(row ?? {}) as [string, ICellData | null][]) {
        if (!data) continue;
        const cell = ws.getCell(Number(r) + 1, Number(c) + 1);
        const richText = data.p?.body?.dataStream?.replace(/\r\n$/, '');
        const value = richText ?? data.v ?? null;
        cell.value = data.f ? { formula: data.f.replace(/^=/, ''), result: value ?? undefined } as ExcelJS.CellFormulaValue : value;
        const style = typeof data.s === 'string' ? snapshot.styles[data.s] : data.s;
        if (!style) continue;
        cell.font = { name: style.ff ?? undefined, size: style.fs, bold: !!style.bl, italic: !!style.it, underline: !!style.ul?.s, strike: !!style.st?.s, color: argb(style.cl) };
        cell.alignment = { horizontal: horizontal[style.ht ?? 0] || undefined, vertical: vertical[style.vt ?? 0] || undefined, wrapText: style.tb === 3 };
        if (argb(style.bg)) cell.fill = { type: 'pattern', pattern: 'solid', fgColor: argb(style.bg) };
        if (style.n) cell.numFmt = style.n.pattern;
        for (const [key, edge] of [['t', 'top'], ['b', 'bottom'], ['l', 'left'], ['r', 'right']] as const) {
          const b = style.bd?.[key];
          if (b && borders[b.s]) cell.border = { ...cell.border, [edge]: { style: borders[b.s], color: argb(b.cl) } };
        }
      }
    }
    for (const [r, data] of Object.entries(sheet.rowData ?? {})) {
      if (!data) continue;
      const row = ws.getRow(Number(r) + 1);
      if (data.h) row.height = data.h * 3 / 4;
      row.hidden = !!data.hd;
    }
    for (const [c, data] of Object.entries(sheet.columnData ?? {})) {
      if (!data) continue;
      const col = ws.getColumn(Number(c) + 1);
      if (data.w) col.width = Math.max(1, (data.w - 5) / 7);
      col.hidden = !!data.hd;
    }
    for (const range of sheet.mergeData ?? []) ws.mergeCells(range.startRow + 1, range.startColumn + 1, range.endRow + 1, range.endColumn + 1);
    if (sheet.freeze) ws.views = [{ state: 'frozen', xSplit: sheet.freeze.xSplit, ySplit: sheet.freeze.ySplit }];
  }
  const bytes = new Uint8Array(await workbook.xlsx.writeBuffer());
  let binary = '';
  for (let i = 0; i < bytes.length; i += 8192) binary += String.fromCharCode(...bytes.subarray(i, i + 8192));
  return btoa(binary);
}
