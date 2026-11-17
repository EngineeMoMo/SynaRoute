export function createRefreshQueue<Input, Output>(
  read: (input: Input) => Promise<Output>,
  apply: (output: Output, input: Input) => void,
  merge: (previous: Input, next: Input) => Input = (_previous, next) => next,
) {
  let pending: Input | undefined;
  let current: Input | undefined;
  let revision = 0;
  let running: Promise<void> | undefined;

  return (input: Input): Promise<void> => {
    revision += 1;
    const previous = pending ?? current;
    pending = previous === undefined ? input : merge(previous, input);
    if (!running) {
      running = Promise.resolve().then(async () => {
        try {
          while (pending !== undefined) {
            current = pending;
            pending = undefined;
            const observed = revision;
            try {
              const output = await read(current);
              if (observed === revision) apply(output, current);
            } catch (error) {
              if (observed === revision) throw error;
            }
          }
        } finally {
          current = undefined;
          running = undefined;
        }
      });
    }
    return running;
  };
}
