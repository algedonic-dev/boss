<script lang="ts">
  import type { EmploymentStatus } from './types';
  // `status` is nullable — identity-first: an employee record can exist
  // before it is onboarded into a status. Render a neutral chip on null.
  let { status } = $props<{ status: EmploymentStatus | null }>();
</script>

{#if status}
  <span class="chip chip-emp chip-emp-{status}">{status.replace(/-/g, ' ')}</span>
{:else}
  <span class="chip chip-emp chip-emp-none">unknown</span>
{/if}
