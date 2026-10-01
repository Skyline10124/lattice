import type * as React from 'react';

/** A tag, one word in capitals. State tones label a state and default to their word; emphasis tones take the consumer's word. */
export type TagTone = 'prompt' | 'warn' | 'error' | 'ok' | 'iris' | 'red' | 'amber' | 'cyan' | 'ink';
export interface TagProps extends React.HTMLAttributes<HTMLSpanElement> {
  /** State: prompt → signal, warn → amber, error → vhs-red, ok → phosphor. Emphasis: iris → signal, red → vhs-red, amber → vhs-amber, cyan → phosphor, ink → ink. Default 'prompt'. */
  tone?: TagTone;
  /** The word: required for emphasis tones; overrides a state tone's default (PROMPT, WARN, ERROR, OK). Keep it one short word. */
  children?: React.ReactNode;
}
export declare function Tag(props: TagProps): React.ReactElement;

/** The container. 'marked': 2px edges broken at the corners around plus marks, space-12 padding. 'plain': a continuous 1px border, space-6 padding. */
export interface FrameProps extends React.HTMLAttributes<HTMLDivElement> {
  /** Default 'marked' — once per page, for the one thing it is about. 'plain' for ordinary panels, which may repeat. */
  variant?: 'marked' | 'plain';
  children?: React.ReactNode;
}
export declare function Frame(props: FrameProps): React.ReactElement;

/** Days, hours, minutes and seconds left until `to`, ticking each second. */
export interface CountdownProps extends Omit<React.HTMLAttributes<HTMLDivElement>, 'children'> {
  /** The target instant: an ISO string, a Date or epoch milliseconds. An invalid value shows "--". */
  to: string | number | Date;
  /** Called once when the countdown reaches zero (also on mount if the target has already passed). */
  onComplete?: () => void;
  /** Unit words under the four numerals, in order; also read out by screen readers. Default ['DAYS', 'HOURS', 'MIN', 'SEC']; for Chinese, ['天', '时', '分', '秒']. */
  labels?: [string, string, string, string];
}
export declare function Countdown(props: CountdownProps): React.ReactElement;

/** A 40px square button. Label in sentence case, verb first. */
export interface ButtonProps extends React.ButtonHTMLAttributes<HTMLButtonElement> {
  /** 'primary' (default): signal fill, at most one per view. 'secondary': control fill, glow on hover, no outline. */
  variant?: 'primary' | 'secondary';
  /** Defaults to 'button', not 'submit'. */
  type?: 'button' | 'submit' | 'reset';
}
export declare function Button(props: ButtonProps): React.ReactElement;

/** A labelled single-line text field. Other props go to the <input>. */
export interface InputProps extends Omit<React.InputHTMLAttributes<HTMLInputElement>, 'className'> {
  /** Field label above, set in the label style (capitals). */
  label?: React.ReactNode;
  /** One line of help below, in text-small. Hidden while `error` is set. */
  hint?: React.ReactNode;
  /** A sentence saying what is wrong; turns the bottom rule and the note vhs-red and sets aria-invalid. */
  error?: React.ReactNode;
  /** Set the value in mono — for paths, IDs, commands. */
  mono?: boolean;
  /** Applied to the wrapper. */
  className?: string;
}
export declare function Input(props: InputProps): React.ReactElement;

/** An on/off switch (role="switch"). Controlled with `checked`, or uncontrolled from `defaultChecked`. */
export interface SwitchProps extends Omit<React.ButtonHTMLAttributes<HTMLButtonElement>, 'onChange'> {
  checked?: boolean;
  defaultChecked?: boolean;
  /** Called with the new state. */
  onChange?: (checked: boolean) => void;
  /** Text to the right of the track; it names what the switch turns on. */
  label?: React.ReactNode;
}
export declare function Switch(props: SwitchProps): React.ReactElement;

/** A native checkbox with a drawn square box. Other props go to the <input>. */
export interface CheckboxProps extends Omit<React.InputHTMLAttributes<HTMLInputElement>, 'type'> {
  label?: React.ReactNode;
}
export declare function Checkbox(props: CheckboxProps): React.ReactElement;

/** A native radio with a drawn square box and a centred square when selected; group radios with the same `name` inside a fieldset. */
export interface RadioProps extends Omit<React.InputHTMLAttributes<HTMLInputElement>, 'type'> {
  label?: React.ReactNode;
}
export declare function Radio(props: RadioProps): React.ReactElement;

/** One choice from two to five options, shown as joined square segments; applies at once. */
export interface SegmentedOption { value: string; label: React.ReactNode; disabled?: boolean }
export interface SegmentedProps extends Omit<React.HTMLAttributes<HTMLDivElement>, 'onChange' | 'defaultValue'> {
  /** Group label above, in the label style (capitals); also names the radiogroup. */
  label?: React.ReactNode;
  /** Plain strings are used as both value and label. */
  options: Array<string | SegmentedOption>;
  /** Controlled selection. */
  value?: string;
  /** Initial selection when uncontrolled; defaults to the first option. */
  defaultValue?: string;
  /** Called with the chosen value. */
  onChange?: (value: string) => void;
  /** Radio group name; generated when omitted. */
  name?: string;
  /** Disables every segment. */
  disabled?: boolean;
}
export declare function Segmented(props: SegmentedProps): React.ReactElement;

/** A labelled multi-line text field, sharing Input's label, hint and error. Other props go to the <textarea>; rows defaults to 4. */
export interface TextareaProps extends Omit<React.TextareaHTMLAttributes<HTMLTextAreaElement>, 'className'> {
  label?: React.ReactNode;
  hint?: React.ReactNode;
  error?: React.ReactNode;
  /** Set the text in mono — for commands, code, logs. */
  mono?: boolean;
  /** Applied to the wrapper. */
  className?: string;
}
export declare function Textarea(props: TextareaProps): React.ReactElement;

/** A labelled native select with a drawn chevron. Other props go to the <select>. */
export interface SelectOption { value: string; label: React.ReactNode; disabled?: boolean }
export interface SelectProps extends Omit<React.SelectHTMLAttributes<HTMLSelectElement>, 'className'> {
  label?: React.ReactNode;
  hint?: React.ReactNode;
  error?: React.ReactNode;
  /** Plain strings are used as both value and label. Or pass <option> children. */
  options?: Array<string | SelectOption>;
  /** An empty, unselectable first choice shown until something is picked. */
  placeholder?: string;
  className?: string;
}
export declare function Select(props: SelectProps): React.ReactElement;

/** A one-line message led by a state tag. 'error' uses role="alert"; the others role="status". */
export interface NoticeProps extends React.HTMLAttributes<HTMLDivElement> {
  /** info → INFO on signal, ok → OK, warn → WARN, error → ERROR. Default 'info'. */
  tone?: 'info' | 'ok' | 'warn' | 'error';
  /** At most one action, usually a secondary Button or a link. */
  action?: React.ReactNode;
  /** One sentence, in text; paths and IDs inside it in <code>. */
  children?: React.ReactNode;
}
export declare function Notice(props: NoticeProps): React.ReactElement;

/** Progress as a row of square cells lit whole, with the percentage in mono (role="progressbar"). */
export interface ProgressProps extends React.HTMLAttributes<HTMLDivElement> {
  /** Ignored when `segments` is given. */
  value?: number;
  /** Default 100. */
  max?: number;
  /** Label above, in the label style; also names the progressbar. */
  label?: React.ReactNode;
  /** Number of cells, default 24; use the item count when it is small (7 of 12 tool calls → 12). */
  cells?: number;
  /** Up to four parts that make up the value, each lit in its series colour with a legend beneath; replaces `value`. Keep each thing on the same series everywhere. */
  segments?: { label: React.ReactNode; value: number; series?: 1 | 2 | 3 | 4 }[];
}
export declare function Progress(props: ProgressProps): React.ReactElement;

/** The VHS bands — vhs-red, vhs-amber, signal. Decorative: never a state, never behind text. */
export interface StripeProps extends React.HTMLAttributes<HTMLDivElement> {
  /** 'band' (default): full width, space-2 bands and gaps, once per screen. 'line': a space-2 tall trim for a page edge. 'spine': vertical bars down the left of the children. */
  variant?: 'band' | 'line' | 'spine';
  /** Only for 'spine': the key figure or title it marks. */
  children?: React.ReactNode;
}
export declare function Stripe(props: StripeProps): React.ReactElement;

/** A data table: label-style head, hairline row rules, mono right-aligned figures. */
export interface TableColumn<Row = Record<string, unknown>> {
  key: string;
  label: React.ReactNode;
  align?: 'left' | 'right';
  /** Mono and tabular — for IDs, counts, durations, sizes. */
  mono?: boolean;
  /** Custom cell, e.g. a Tag for a status column. */
  render?: (row: Row) => React.ReactNode;
}
export interface TableProps<Row = Record<string, unknown>> extends React.TableHTMLAttributes<HTMLTableElement> {
  columns: TableColumn<Row>[];
  rows: Row[];
  caption?: React.ReactNode;
  /** Field used as the React key of each row; defaults to the index. */
  rowKey?: string;
}
export declare function Table<Row = Record<string, unknown>>(props: TableProps<Row>): React.ReactElement;

/** One of the IBM Carbon icons shipped with the system (Apache-2.0). */
export type IconName = 'add' | 'arrow--left' | 'arrow--right' | 'bot' | 'branch' | 'calendar' | 'chat' | 'checkmark' | 'chevron--down' | 'chevron--left' | 'chevron--right' | 'chevron--up' | 'close' | 'cloud' | 'code' | 'commit' | 'copy' | 'data--base' | 'document' | 'download' | 'edit' | 'filter' | 'flash' | 'folder' | 'grid' | 'information--square' | 'launch' | 'link' | 'list' | 'locked' | 'overflow-menu--horizontal' | 'pause' | 'play' | 'renew' | 'restart' | 'save' | 'search' | 'send' | 'settings' | 'stop' | 'subtract' | 'terminal' | 'time' | 'tool-kit' | 'trash-can' | 'unlocked' | 'upload' | 'view' | 'view--off' | 'warning--alt';
/** A Carbon icon drawn in the current text colour (fill: currentColor). */
export interface IconProps extends Omit<React.SVGAttributes<SVGSVGElement>, 'name'> {
  name: IconName;
  /** 16 (text-small, tables), 20 (body, buttons; default), 24 or 32 (empty states, beside large headings). */
  size?: 16 | 20 | 24 | 32;
  /** Spoken text. Without it the icon is decorative (aria-hidden); an icon-only button needs one. */
  label?: string;
}
export declare function Icon(props: IconProps): React.ReactElement | null;

declare global {
  interface Window { LatticeSignal: { Tag: typeof Tag; Frame: typeof Frame; Countdown: typeof Countdown; Button: typeof Button; Input: typeof Input; Switch: typeof Switch; Checkbox: typeof Checkbox; Radio: typeof Radio; Segmented: typeof Segmented; Textarea: typeof Textarea; Select: typeof Select; Notice: typeof Notice; Progress: typeof Progress; Table: typeof Table; Icon: typeof Icon; Stripe: typeof Stripe } }
}
