<script lang="ts">
  // An edge that shows both the history and the moment.
  //
  // Stroke width is cumulative handoffs on this route — the shape of
  // the machine over the whole window, unchanged from before. The
  // pulses are what is happening NOW: one dot travelling source to
  // target for each handoff observed since the last poll.
  //
  // That split is the whole request. "We should still have the
  // history of the hand-off visualized by thickness like we currently
  // do. I am just imagining a pulse traversing the edge for each
  // hand-off happening in real-time to show how fast the system is
  // operating now that we are on a real timeline."
  //
  // It only became worth building when the brewery moved to wallclock.
  // Under warp the sim ran ~1000x real time, so a "pulse per handoff"
  // would have been a solid stripe carrying no information. At 1x, the
  // rate on screen IS the rate of the business.
  import { BaseEdge, getBezierPath, getSmoothStepPath } from '@xyflow/svelte';

  let {
    id,
    sourceX,
    sourceY,
    targetX,
    targetY,
    sourcePosition,
    targetPosition,
    source,
    target,
    label,
    style,
    markerEnd,
    data,
  } = $props<{
    id: string;
    sourceX: number;
    sourceY: number;
    targetX: number;
    targetY: number;
    sourcePosition: any;
    targetPosition: any;
    source: string;
    target: string;
    label?: string;
    style?: string;
    markerEnd?: string;
    data?: {
      pulses?: number;
      pulseToken?: number;
      colour?: string;
    };
  }>();

  const selfLoop = $derived(source === target);

  /// The path, and the label anchor.
  ///
  /// A self-edge is drawn by hand. Both of xyflow's path helpers
  /// assume the ends are different points, and an intra-department
  /// handoff has them on the same node — the built-in output is a
  /// near-zero-length line. These are not an edge case here: the
  /// dispatcher's loop is the single busiest route on the map.
  const geometry = $derived.by(() => {
    if (selfLoop) {
      const lift = 90;
      const spread = 70;
      return {
        path:
          `M ${sourceX},${sourceY} ` +
          `C ${sourceX + spread},${sourceY - lift} ` +
          `${targetX - spread},${targetY - lift} ` +
          `${targetX},${targetY}`,
        labelX: (sourceX + targetX) / 2,
        labelY: sourceY - lift * 0.75,
      };
    }
    const [path, labelX, labelY] = getBezierPath({
      sourceX,
      sourceY,
      targetX,
      targetY,
      sourcePosition,
      targetPosition,
    });
    return { path, labelX, labelY };
  });

  // Never more than this many dots on one edge in one tick. A burst of
  // forty handoffs is information ("that route is hot"), forty dots is
  // not — they overlap into a line and read as one long smear. The
  // count is already on the label; this is for RHYTHM.
  const MAX_PULSES = 6;
  const pulseCount = $derived(Math.min(data?.pulses ?? 0, MAX_PULSES));

  // Slightly slower than the 5s poll so a dot is still in flight when
  // the next batch arrives — the map reads as continuous movement
  // rather than a stutter every five seconds.
  const TRAVEL_S = 2.2;

  let reduceMotion = $state(false);
  $effect(() => {
    // Not a decoration: someone who has asked the OS to stop moving
    // things should get the counts and the thickness, and no dots.
    const mq = window.matchMedia('(prefers-reduced-motion: reduce)');
    reduceMotion = mq.matches;
    const on = () => (reduceMotion = mq.matches);
    mq.addEventListener('change', on);
    return () => mq.removeEventListener('change', on);
  });
</script>

<BaseEdge {id} path={geometry.path} {label} labelX={geometry.labelX} labelY={geometry.labelY} {style} {markerEnd} />

{#if pulseCount > 0 && !reduceMotion}
  <!-- Keyed on the token so a new poll REPLAYS the animation. Without
       it Svelte updates the count in place and `animateMotion`, which
       only begins on mount, never fires again — the first tick would
       animate and every tick after would be still. -->
  {#key data?.pulseToken}
    {#each Array(pulseCount) as _, i (i)}
      <circle
        class="os-pulse"
        r="4"
        fill={data?.colour ?? '#0f766e'}
      >
        <animateMotion
          dur={`${TRAVEL_S}s`}
          path={geometry.path}
          repeatCount="1"
          fill="remove"
          begin={`${i * 0.18}s`}
        />
        <!-- Fade at the end so a dot arriving at the node does not
             vanish mid-stride. -->
        <animate
          attributeName="opacity"
          values="0;1;1;0"
          keyTimes="0;0.08;0.85;1"
          dur={`${TRAVEL_S}s`}
          repeatCount="1"
          fill="remove"
          begin={`${i * 0.18}s`}
        />
      </circle>
    {/each}
  {/key}
{/if}

<style>
  .os-pulse {
    /* Sits above the edge stroke but below the node cards. */
    pointer-events: none;
    filter: drop-shadow(0 0 3px currentColor);
    opacity: 0;
  }
</style>
