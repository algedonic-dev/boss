<script lang="ts">
  // Compact section with a title — mirrors the React Section primitive
  // in apps/web/src/ui/index.tsx. Classes match so existing
  // styles.css renders unchanged.
  let { title, wide = false, children } = $props<{
    title: string;
    wide?: boolean;
    children: () => any;
  }>();
</script>

<section class="tab-section {wide ? 'tab-section-wide' : ''}">
  <h3 style="margin-top: 0">{title}</h3>
  {@render children()}
</section>
