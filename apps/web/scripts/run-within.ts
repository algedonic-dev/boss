// A CHILD PROCESS WITH A BUDGET OF ITS OWN, AND A FAILURE THAT CAN BE READ
// (backlog 3f421243, 2026-11-06).
//
// Train gate c773d182 (red train 5283152e) killed
// scripts/a-direct-mocked-run-refuses-a-foreign-server.test.ts at its
// 300 s test timeout and recorded nothing else: the pin awaited each
// child's stdout and stderr to their END, so when bun's timeout fired
// everything the children had printed — the mocked runner's
// `[mocked-runner] …` stage lines, Playwright's — died unread. Which stage
// the forge-ci leg sat in (the dev-server's bind, its bundle, the browser)
// could not be learned from the record, only guessed.
//
// So a leg reads its output as it arrives and holds a budget below its
// test's. A child still running at the budget is sent SIGTERM (the mocked
// runner stops its dev-server on it) and the leg fails naming itself, the
// command, the budget and EVERYTHING the child printed — not a tail: its
// last line is the stage it was in, and the lines before are how it got
// there (CLAUDE.md §Diagnosis, "Quiet is not free").

type Env = Readonly<Record<string, string | undefined>>;

export type LegResult = Readonly<{ code: number; output: string }>;

/// Run `command` to its exit within `budgetMs`, its stdout and stderr in
/// one string in the order they arrived. Rejects, after killing it, when
/// the child or a process holding its output outlives the budget.
export async function runWithin(
  leg: string,
  command: ReadonlyArray<string>,
  options: Readonly<{ cwd: string; env: Env; budgetMs: number }>,
): Promise<LegResult> {
  const child = Bun.spawn([...command], {
    cwd: options.cwd,
    env: { ...options.env },
    stdout: 'pipe',
    stderr: 'pipe',
    stdin: 'ignore',
  });
  let output = '';
  const drain = async (stream: ReadableStream<Uint8Array>): Promise<void> => {
    const decoder = new TextDecoder();
    for await (const chunk of stream) output += decoder.decode(chunk, { stream: true });
  };
  const drained = Promise.all([drain(child.stdout), drain(child.stderr)]);
  let timer: ReturnType<typeof setTimeout> | undefined;
  const deadline = new Promise<'deadline'>((resolve) => {
    timer = setTimeout(() => resolve('deadline'), options.budgetMs);
  });
  const said = (what: string): string =>
    `${leg}: ${command.join(' ')} ${what} after ${options.budgetMs} ms. ` +
    `Everything it printed, in arrival order — the last line is where it was:\n${output}`;
  try {
    const exited = await Promise.race([child.exited, deadline]);
    if (exited === 'deadline') {
      child.kill('SIGTERM');
      throw new Error(said('had not exited'));
    }
    // A process the child started can hold the pipe after the child is
    // gone; that is a wait too, and it is named the same way.
    if ((await Promise.race([drained, deadline])) === 'deadline') {
      throw new Error(said(`exited ${exited}, but its output was still held open`));
    }
    return { code: exited, output };
  } finally {
    clearTimeout(timer);
  }
}
