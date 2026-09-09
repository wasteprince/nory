export function subscriptionFromPaste(text: string): string | null {
  const value = text.trim();
  if (!value || /\s/.test(value)) return null;
  try {
    const url = new URL(value);
    return ["https:", "http:"].includes(url.protocol) && url.hostname
      && !url.username && !url.password ? value : null;
  } catch { return null; }
}

export function isEditing(target: EventTarget | null): boolean {
  return target instanceof Element && !!target.closest(
    'input, textarea, select, [contenteditable]:not([contenteditable="false"]), [role="textbox"]',
  );
}
