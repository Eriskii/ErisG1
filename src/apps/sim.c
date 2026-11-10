/* A simulated g1v0 arm for previews and tests: scouter_kit's VM core (firmware/scouter_kit/src/vm) on a plain
 * arena, with the platform interface (vm.h, struct vm_host) answered here. Time is whatever the caller sets;
 * sensors are properties the caller sets; the panel is a 640 x 480 array of levels. One thread per sim. */
#include <stdlib.h>
#include <string.h>

#include "vm.h"

#define QUEUE 512
#define PROPS 0x40

struct sim {
	struct vm vm;
	struct vm_host host;
	uint8_t arena[VM_ARENA_SIZE];
	uint8_t panel[VM_PANEL_H][VM_PANEL_W];
	int panel_on;
	uint32_t panel_frames;
	uint32_t now;
	/* host packets waiting: seq + code, each malloc'd */
	uint8_t *packets[QUEUE];
	uint32_t lengths[QUEUE];
	uint32_t head, tail;
	uint16_t seq;
	uint32_t props[PROPS];
	uint32_t fired;
	/* events: type, then the body from offset 9 */
	uint8_t events[QUEUE][64];
	uint32_t event_lengths[QUEUE];
	uint32_t event_head, event_tail;
	/* the last PEER_SEND write not yet taken */
	uint32_t peer_value, peer_time;
	int peer_pending;
};

static uint32_t sim_now(void *ctx) { return ((struct sim *)ctx)->now; }

static const uint8_t *sim_packet(void *ctx, uint32_t *len)
{
	struct sim *s = ctx;
	if (s->head == s->tail)
		return NULL;
	*len = s->lengths[s->head % QUEUE];
	return s->packets[s->head % QUEUE];
}

static void sim_packet_done(void *ctx)
{
	struct sim *s = ctx;
	free(s->packets[s->head % QUEUE]);
	s->head++;
}

static uint32_t sim_poll(void *ctx)
{
	struct sim *s = ctx;
	uint32_t fired = s->fired;
	s->fired = 0;
	return fired;
}

static uint32_t sim_prop(void *ctx, uint32_t id) { return id < PROPS ? ((struct sim *)ctx)->props[id] : VM_UNKNOWN; }

static int sim_event(void *ctx, uint8_t type, const uint8_t *body, uint32_t len)
{
	struct sim *s = ctx;
	if (s->event_tail - s->event_head >= QUEUE || len > 63)
		return 1;
	uint8_t *e = s->events[s->event_tail % QUEUE];
	e[0] = type;
	memcpy(e + 1, body, len);
	s->event_lengths[s->event_tail % QUEUE] = len + 1;
	s->event_tail++;
	return 0;
}

static int sim_panel_begin(void *ctx, int32_t x, int32_t y, int32_t w, int32_t h)
{
	struct sim *s = ctx;
	(void)x, (void)y, (void)w, (void)h;
	if (!s->panel_on) {
		memset(s->panel, 0, sizeof s->panel);
		s->panel_on = 1;
		s->props[VM_P_PANEL] = 1;
	}
	return 0;
}

static void sim_panel_row(void *ctx, int32_t x, int32_t y, int32_t w, const uint8_t *px)
{
	struct sim *s = ctx;
	for (int32_t i = 0; i < w; i++)
		s->panel[y][x + i] = i % 2 ? px[i / 2] & 15 : px[i / 2] >> 4;
}

static void sim_panel_end(void *ctx) { ((struct sim *)ctx)->panel_frames++; }

static void sim_panel_power(void *ctx, uint32_t on)
{
	struct sim *s = ctx;
	s->props[VM_P_PANEL] = on;
	if (on != 1 || !s->panel_on)
		memset(s->panel, 0, sizeof s->panel);
	s->panel_on = on == 1;
}

static void sim_brightness(void *ctx, uint32_t level) { ((struct sim *)ctx)->props[VM_P_BRIGHTNESS] = level; }

static void sim_imu_rate(void *ctx, uint32_t hz) { (void)ctx, (void)hz; }

static int sim_prop_write(void *ctx, uint32_t id, uint32_t value)
{
	struct sim *s = ctx;
	switch (id) {
	case VM_P_GYRO_RATE:
	case VM_P_AUTO_BRIGHTNESS:
		s->props[id] = value;
		return 0;
	case VM_P_PEER_LINK:
		/* the other simulated arm is always there: on reads 2 */
		s->props[id] = value ? 2 : 0;
		return 0;
	case VM_P_PEER_SEND:
		if (!s->props[VM_P_PEER_LINK])
			return 5;
		s->peer_value = value;
		s->peer_time = s->now;
		s->peer_pending = 1;
		return 0;
	}
	return 5;
}

struct sim *sim_new(int left)
{
	struct sim *s = calloc(1, sizeof *s);
	if (!s)
		return NULL;
	s->host = (struct vm_host){ s, sim_now, sim_packet, sim_packet_done, sim_poll, sim_prop, sim_event,
				    sim_panel_begin, sim_panel_row, sim_panel_end, sim_panel_power, sim_brightness,
				    sim_imu_rate, sim_prop_write, NULL, NULL };
	for (int i = 0; i < PROPS; i++)
		s->props[i] = VM_UNKNOWN;
	s->props[VM_P_ARM] = left ? 1 : 0;
	s->props[VM_P_LINK] = 1;
	s->props[VM_P_LINK_INTERVAL] = 15000;
	s->props[VM_P_BRIGHTNESS] = 21;
	s->props[VM_P_PANEL] = 0;
	s->props[VM_P_GYRO_RATE] = 0;
	s->props[VM_P_AUTO_BRIGHTNESS] = 0;
	s->props[VM_P_PEER_LINK] = 0;
	s->props[VM_P_TOUCH_GESTURE] = 0;
	vm_init(&s->vm, s->arena, &s->host, 0);
	return s;
}

void sim_free(struct sim *s)
{
	while (s->head != s->tail)
		sim_packet_done(s);
	free(s);
}

void sim_set_time(struct sim *s, uint32_t now) { s->now = now; }

/* Queues host code as the next packet. */
void sim_push(struct sim *s, const uint8_t *code, uint32_t len)
{
	uint8_t *p = malloc(len + 2);
	p[0] = (uint8_t)s->seq;
	p[1] = (uint8_t)(s->seq >> 8);
	memcpy(p + 2, code, len);
	s->packets[s->tail % QUEUE] = p;
	s->lengths[s->tail % QUEUE] = len + 2;
	s->tail++;
	s->seq++;
}

/* Packets queued and not yet run. */
uint32_t sim_waiting(const struct sim *s) { return s->tail - s->head; }

uint32_t sim_run(struct sim *s) { return vm_run(&s->vm); }

void sim_set_prop(struct sim *s, uint32_t id, uint32_t value)
{
	if (id < PROPS)
		s->props[id] = value;
}

int sim_get_prop(const struct sim *s, uint32_t id, uint32_t *value) { return vm_prop(&s->vm, id, value); }

void sim_touch(struct sim *s, uint32_t gesture)
{
	s->props[VM_P_TOUCH_GESTURE] = gesture;
	vm_trigger(&s->vm, VM_T_TOUCH, gesture);
}

/* A message from the other arm, sent at `sent` in this arm's clock. */
void sim_peer(struct sim *s, uint32_t value, uint32_t sent)
{
	s->props[VM_P_PEER_MSG] = value;
	s->props[VM_P_PEER_T_US] = sent;
	vm_trigger(&s->vm, VM_T_PEER, value);
}

/* The last PEER_SEND write, once: 1 and its value and time, or 0. */
int sim_take_peer(struct sim *s, uint32_t *value, uint32_t *time)
{
	if (!s->peer_pending)
		return 0;
	s->peer_pending = 0;
	*value = s->peer_value;
	*time = s->peer_time;
	return 1;
}

int sim_start(struct sim *s, uint32_t table) { return vm_start(&s->vm, table); }
void sim_stop(struct sim *s) { vm_stop(&s->vm); }
void sim_status(const struct sim *s, uint8_t *out) { vm_status(&s->vm, out); }
void sim_events(struct sim *s, uint16_t mask) { vm_set_events(&s->vm, mask); }

/* The next event (type then body), or 0. */
uint32_t sim_next_event(struct sim *s, uint8_t *out)
{
	if (s->event_head == s->event_tail)
		return 0;
	uint32_t len = s->event_lengths[s->event_head % QUEUE];
	memcpy(out, s->events[s->event_head % QUEUE], len);
	s->event_head++;
	return len;
}

const uint8_t *sim_panel(const struct sim *s) { return &s->panel[0][0]; }
int sim_panel_on(const struct sim *s) { return s->panel_on; }
uint32_t sim_panel_frames(const struct sim *s) { return s->panel_frames; }
uint8_t *sim_arena(struct sim *s) { return s->arena; }
