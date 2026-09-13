<script setup lang="ts">
import { status } from "./api";
import { duration } from "./format";
</script>
<template>
  <span class="timer">{{ duration(status.seconds) }}</span>
</template>
