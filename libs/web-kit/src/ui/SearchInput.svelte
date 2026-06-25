<script lang="ts">
  let { value = $bindable(''), placeholder = '' } = $props<{
    value?: string;
    placeholder?: string;
  }>();
</script>

<input
  type="search"
  class="filter-input"
  {placeholder}
  bind:value
/>
