// A READING WITHHELD BY POLICY SCOPE, as the page says it (backlog
// bd506215). Since d0058c92 and 0964ba80 the server refuses, by name,
// the reads a narrowed scope may not make — the conductor's heartbeat,
// the board rule's last firing, the map's machine firings and next-up
// sources, the routes' observed counts, the rules' firing record and the
// dispatcher's schedule — because those records are not scoped by
// packet. The web used to draw each refusal as a failure: the conductor
// lamp read "no firing on record — liveness unknown" in WARN tone, a
// refusal in an alarm's colour. A refusal is neither trouble nor
// unknown; it is the policy working, and every surface says so in these
// words, in a neutral tone.
//
// WHICH reading is withheld is the SERVER's structured answer — a
// `withheld` flag (a border machine, a next-up row, the rule-firings and
// dispatcher-schedule payloads) or a `withheld` machine state on the
// regions — never recognised here from the words of a reason (backlog
// 1805bac0; the phrase matcher that did so is deleted, CLAUDE.md 9a).

/** What every withheld reading prints where its value would stand. */
export const NOT_IN_SCOPE = 'not in your policy scope';
