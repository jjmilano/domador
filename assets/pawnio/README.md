# Módulo PawnIO incluido

`AMDFamily17.bin` viene sin modificar del release oficial firmado de
[PawnIO.Modules](https://github.com/namazso/PawnIO.Modules) 0.2.11
(`release_0_2_11.zip`, SHA-256 del zip `43608CB89BC84247FEF1368A139013F7D043E17DB6D6C8DFC9B46BF0905A81F4`).

Licencia: LGPL-2.1-or-later (c) namazso. Fuente: `AMDFamily17.p` en ese repositorio.

Domador sólo usa `ioctl_read_smn` para leer el sensor de temperatura del CPU
(`THM_TCON_CUR_TMP`, SMN `0x00059800`). Es lectura: no escribe MSR ni SMN.
Requiere que el usuario instale el driver PawnIO (`winget install namazso.PawnIO`).
