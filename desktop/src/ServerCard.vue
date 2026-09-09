<script setup lang="ts">
import { Globe2, Check } from "@lucide/vue";
import { flag, cleanName } from "./format";
import type { Profile } from "./types";
import ProtocolBadges from "./ProtocolBadges.vue";
defineProps<{ profile: Profile; selected: boolean; disabled: boolean }>();
defineEmits<{ select: [] }>();
</script>
<template>
  <button
    type="button"
    class="server-card group"
    :class="{ selected }"
    :disabled="disabled"
    :aria-pressed="selected"
    @click="$emit('select')"
  >
    <div class="flex items-center gap-2.5 min-w-0">
      <img
        v-if="profile.flag_image"
        :src="profile.flag_image"
        class="flag"
        alt=""
      /><span v-else-if="flag(profile.name)" class="emoji">{{
        flag(profile.name)
      }}</span
      ><Globe2 v-else :size="19" class="shrink-0" /><span
        class="server-name"
        :title="cleanName(profile.name)"
        >{{ cleanName(profile.name) }}</span
      ><Check
        v-if="selected"
        :size="15"
        class="ml-auto shrink-0 text-accent-text"
      />
    </div>
    <p
      class="server-description"
      :title="profile.description ?? ''"
    >
      {{ profile.description || " " }}
    </p>
    <div class="flex min-w-0 items-center gap-1.5 mt-auto">
      <ProtocolBadges :profile="profile" /><span
        class="latency"
        :class="{
          'is-fast': profile.latency_ms !== null && profile.latency_ms < 100,
        }"
        >{{
          profile.latency_ms === null ? "n/a" : `${profile.latency_ms} мс`
        }}</span
      >
    </div>
  </button>
</template>
