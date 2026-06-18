<script lang="ts">
  // Client-side navigation link. Mirrors apps/web/src/ui/index.tsx Link.
  import { navigate } from '../router';

  let { to, className = '', children } = $props<{
    to: string;
    className?: string;
    children: () => any;
  }>();

  function onClick(e: MouseEvent) {
    // Allow modified clicks (cmd/ctrl/shift) to fall through to the
    // browser's default open-in-new-tab behavior.
    if (e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;
    e.preventDefault();
    navigate(to);
  }
</script>

<a href={to} class={className} onclick={onClick}>{@render children()}</a>
