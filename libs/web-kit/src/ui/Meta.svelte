<script lang="ts">
  // Label / value pair used in device/account/vendor hero strips.
  let {
    label,
    mono = false,
    children,
  } = $props<{
    label: string;
    mono?: boolean;
    children: () => any;
  }>();
</script>

<div class="detail-meta-item">
  <div class="detail-meta-label">{label}</div>
  <div class="detail-meta-value {mono ? 'mono' : ''}">{@render children()}</div>
</div>
