<script setup lang="ts">
import { protocolBadges } from "./protocol";
import type { Profile } from "./types";
defineProps<{ profile: Profile }>();
</script>

<template>
  <div class="profile-badges">
    <span v-for="badge in protocolBadges(profile)" :key="badge.kind"
      :class="badge.kind === 'protocol' || badge.kind === 'format' ? 'protocol' : 'transport'">
      {{ badge.label }}
    </span>
  </div>
</template>
