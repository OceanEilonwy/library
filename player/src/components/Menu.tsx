// An overflow ("⋮") menu: a button that opens a list of actions.
// Arrow keys move between items, Escape closes and returns focus.

import { For, Show, createSignal, onSettled } from 'solid-js';
import { Portal } from '@solidjs/web';
import { Icon, type IconName } from './Icon';

export type MenuItem = {
  label: string;
  icon?: IconName;
  onSelect: () => void;
  danger?: boolean;
  disabled?: boolean;
};

export function Menu(props: { label: string; items: readonly MenuItem[]; icon?: IconName; class?: string }) {
  const [open, setOpen] = createSignal(false);
  const [position, setPosition] = createSignal<{ top: number; left: number; up: boolean }>({
    top: 0,
    left: 0,
    up: false,
  });
  let button: HTMLButtonElement | undefined;
  let menu: HTMLDivElement | undefined;

  const items = () => menu?.querySelectorAll<HTMLButtonElement>('[role="menuitem"]:not([disabled])') ?? [];

  function show() {
    if (!button) return;
    const rect = button.getBoundingClientRect();
    const up = rect.bottom + 320 > window.innerHeight && rect.top > 320;
    setPosition({ top: up ? rect.top : rect.bottom, left: rect.right, up });
    setOpen(true);
    requestAnimationFrame(() => items()[0]?.focus());
  }

  function close(returnFocus = true) {
    setOpen(false);
    if (returnFocus) button?.focus();
  }

  function onKeyDown(event: KeyboardEvent) {
    const list = Array.from(items());
    const index = list.indexOf(document.activeElement as HTMLButtonElement);
    switch (event.key) {
      case 'ArrowDown':
        event.preventDefault();
        list[(index + 1) % list.length]?.focus();
        break;
      case 'ArrowUp':
        event.preventDefault();
        list[(index - 1 + list.length) % list.length]?.focus();
        break;
      case 'Home':
        event.preventDefault();
        list[0]?.focus();
        break;
      case 'End':
        event.preventDefault();
        list[list.length - 1]?.focus();
        break;
      case 'Escape':
        event.preventDefault();
        close();
        break;
      case 'Tab':
        close(false);
        break;
      default:
        break;
    }
  }

  onSettled(() => {
    const onPointerDown = (event: PointerEvent) => {
      if (!open()) return;
      const target = event.target as Node;
      if (menu?.contains(target) || button?.contains(target)) return;
      close(false);
    };
    const onScroll = () => open() && close(false);
    document.addEventListener('pointerdown', onPointerDown);
    window.addEventListener('resize', onScroll);
    return () => {
      document.removeEventListener('pointerdown', onPointerDown);
      window.removeEventListener('resize', onScroll);
    };
  });

  return (
    <>
      <button
        ref={(el) => (button = el)}
        type="button"
        class={['icon-button', props.class]}
        aria-label={props.label}
        aria-haspopup="menu"
        aria-expanded={open() ? 'true' : 'false'}
        onClick={(event) => {
          event.stopPropagation();
          if (open()) close();
          else show();
        }}
      >
        <Icon name={props.icon ?? 'more'} />
      </button>
      <Show when={open()}>
        <Portal>
          <div class="menu-backdrop" onClick={() => close()} />
          <div
            ref={(el) => (menu = el)}
            class={['menu', { 'menu-up': position().up }]}
            role="menu"
            aria-label={props.label}
            style={{ '--menu-top': `${position().top}px`, '--menu-right': `${window.innerWidth - position().left}px` }}
            onKeyDown={onKeyDown}
          >
            <For each={props.items}>
              {(item) => (
                <button
                  type="button"
                  role="menuitem"
                  class={['menu-item', { danger: !!item.danger }]}
                  disabled={item.disabled}
                  onClick={() => {
                    close();
                    item.onSelect();
                  }}
                >
                  <Show when={item.icon}>{(icon) => <Icon name={icon()} size={20} />}</Show>
                  <span>{item.label}</span>
                </button>
              )}
            </For>
          </div>
        </Portal>
      </Show>
    </>
  );
}
