# Phase 10 mini-report: virtio-blk

| Field | Value |
|---|---|
| Item | virtio-blk (Phase 10 stretch goal 6, `docs/ROADMAP.md`) |
| Status | Complete |
| Date | 2026-09-27 |
| Commit | the commit adding this report |

## 1. Summary
`bridgev boot --disk disk.img` gives the guest a block device. The stock Ubuntu kernel finds it through the devicetree as `virtio_mmio@10001000`, binds its built-in `virtio_blk` driver, and exposes `/dev/vda`:
```
[    0.741336] virtio_blk virtio0: 1/0/0 default/read/poll queues
[    0.746717] virtio_blk virtio0: [vda] 16384 512-byte logical blocks (8.39 MB/8.00 MiB)
/ # mkdir /mnt && mount -t ext2 /dev/vda /mnt && cat /mnt/hello.txt
[    1.253480] EXT4-fs (vda): mounting ext2 file system using the ext4 subsystem
hello from virtio-blk
/ # echo written by bridgev > /mnt/new.txt && ls /mnt && umount /mnt && echo unmounted
hello.txt   lost+found  new.txt
unmounted
```
The file written by the guest is on the host image afterwards (`debugfs -R "cat /new.txt" disk.img` → `written by bridgev`). `--stats` for that session: 52 requests, 338 sectors read, 72 written.

## 2. What was built (`src/system/virtio_blk.rs`)
- **Transport:** virtio-mmio version 2 (virtio spec v1.2 §4.2), covering the register block from MagicValue through QueueReady, QueueNotify, InterruptStatus/ACK, Status (write 0 = reset), the 64-bit queue addresses, and config space `capacity` (in 512-byte sectors).
- **Features:** only `VIRTIO_F_VERSION_1`. There is one queue of at most 128 entries.
- **Requests** (§5.2.6): split virtqueue (§2.7). The descriptor chain is header {type, sector} → data buffers → status byte. Supported types:
  - IN: `pread` into guest memory;
  - OUT: guest memory → `pwrite`;
  - FLUSH: `fdatasync`;
  - GET_ID;
  - others answer UNSUPP.

  Each served chain gets a used-ring entry {head, bytes written}, then `used.idx` is bumped and InterruptStatus bit 0 is set. The PLIC line (source 1) is high while InterruptStatus ≠ 0.
- **Where the work happens:** MMIO writes only record state. The machine loop serves the queue at the start of every slice, and a WFI does not idle while a notification is pending. The DMA goes through `DirectMem::write_bytes`/`load`, so a disk read into a page that held translated code invalidates it (D49). With a raw host pointer, it would silently leave stale translations.
- **Wiring:** devicetree node `virtio_mmio@10001000` (`compatible = "virtio,mmio"`, `interrupts = <1>`), and the `--disk` flag.

## 3. Evidence
- **`linux_boot::linux_mounts_a_virtio_disk_jit`:** creates an ext2 image with `mke2fs -d`, boots, mounts it, reads a host-written file, writes a file, unmounts, powers off, and checks the file on the host with `debugfs`. Passes; run in CI.
- **`fdt::tests::dtc_accepts_the_blob`:** now includes the virtio node, and `dtc` reports no warnings.

## 4. Limitations
- **Device model:** one device and one queue. No `VIRTIO_BLK_F_*` feature bits (size/segment limits, read-only, discard, multi-queue) and no indirect descriptors (the driver does not use them without `VIRTIO_F_INDIRECT_DESC`).
- **Timing:** requests complete at the next slice boundary (≤ 100 k guest instructions), synchronously in the machine thread.
