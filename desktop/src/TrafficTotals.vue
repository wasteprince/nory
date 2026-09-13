<script setup lang="ts">
import { ArrowDownLeft, ArrowUpRight } from "@lucide/vue";
import { status } from "./api";
import { bytes } from "./format";
</script>
<template>
  <div class="traffic-grid card">
    <section class="traffic-card">
      <span><ArrowDownLeft :size="16" />Получено</span>
      <strong>{{ bytes(status.download) }}</strong>
    </section>
    <section class="traffic-card">
      <span><ArrowUpRight :size="16" />Отправлено</span>
      <strong>{{ bytes(status.upload) }}</strong>
    </section>
  </div>
</template>
