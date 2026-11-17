export function createVisibleEventGate(
  refresh: () => void,
  visible: () => boolean,
) {
  let dirty = false;
  return {
    notify(immediate = false) {
      if (immediate || visible()) refresh();
      else dirty = true;
    },
    resume() {
      if (!dirty || !visible()) return;
      dirty = false;
      refresh();
    },
  };
}
