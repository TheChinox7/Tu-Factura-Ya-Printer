# Tu Factura Ya Printer 1.0.4

El agente recoge los tickets de Ventas y del POS destinados a este equipo y los envía a la impresora seleccionada. Cerrar la ventana oculta la configuración. El icono de Tu Factura Ya en la bandeja permite volver a abrirla o salir del agente.

## Probar una impresora

1. En macOS, extrae el ZIP y abre **Tu Factura Ya Printer.app**. En Windows, extrae el ZIP y abre el `.exe`. En Linux, extrae el `.tar.gz` y ejecuta `tu-factura-ya-printer` o usa `./install-launcher.sh` para añadirlo al menú con su icono. Usa una ubicación estable antes de habilitar el inicio automático.
2. Selecciona la impresora. La predeterminada del equipo aparece seleccionada inicialmente si está disponible.
3. Elige **Controlador del sistema** para probar con una impresora convencional o **Térmica ESC/POS** para una térmica compatible. Selecciona 58 u 80 mm para los tickets. Si usas el controlador del sistema para imprimir tickets, configura en la cola del sistema el tamaño de papel de la impresora.
4. Pulsa **Imprimir prueba**. El botón funciona sin configurar la conexión con Laravel.
5. Marca **Iniciar en segundo plano al iniciar sesión** si quieres que vuelva a abrirse tras reiniciar y entrar a tu cuenta. Windows usa la sesión del cajero para acceder a sus impresoras y al Administrador de credenciales.

## Conectar al POS

1. Aplica las migraciones en el servidor Laravel y ejecuta el programador habitual de Laravel para la conciliación del SRI. La cola de impresión usa la base de datos de la aplicación.
2. En **Administración → Sistema → API Keys**, crea una clave estándar llamada, por ejemplo, **Impresiones**. Puedes usarla en todos los equipos que se conecten al mismo dominio. Las claves de integración de Restaurante ya emitidas también sirven.
3. En cada equipo, introduce la URL final del sistema y pega la misma clave. Selecciona una impresora instalada y guarda la configuración. Cada equipo queda registrado por separado. En un servidor de desarrollo en el mismo equipo puedes usar `http://127.0.0.1:8001`. Desde otro equipo usa el dominio HTTPS al que tenga acceso.
4. En **Administración → Sistema → Impresión**, habilita **Impresión directa en Ventas y POS** y guarda el cambio. Recarga las páginas abiertas en cada caja.
5. Imprime un ticket en Ventas o cobra una venta en el POS. El navegador detecta Printer en esa computadora y se conecta automáticamente. Si el navegador pide acceso al dispositivo local, permítelo para ese dominio.
6. Para reproducir el formato del ticket del navegador, selecciona **Controlador del sistema** y el tamaño de papel en el controlador de la impresora. **Enviado a la impresora** confirma que el sistema operativo aceptó el trabajo.

En **Ventas**, pulsa **Imprimir Ticket** en una factura, nota de crédito, nota de débito o guía de remisión autorizada. **Controlador del sistema** imprime el mismo PDF térmico que abre el navegador. Cuando Laravel recibe el ticket, el navegador avisa a Printer en esta computadora para que consulte la cola de inmediato. Si el aviso local no llega, Printer consulta periódicamente la cola.

Si varias cajas usan la misma impresora de red, selecciónala en cada una. Si una caja tiene su propia impresora, selecciona esa. Las estaciones ya vinculadas con claves por punto de emisión siguen siendo compatibles.

El campo de clave es enmascarado y queda vacío al reabrir Ajustes. La credencial de cada equipo se guarda en el Llavero de macOS, el Administrador de credenciales de Windows o un Secret Service de escritorio en Linux mediante `secret-tool`. Laravel guarda hashes de las credenciales.

## Revisar la interfaz en desarrollo

Ejecuta `python3 dev.py` desde la carpeta del proyecto con Rust y Python 3 instalados. Al guardar cambios en `src/`, `Cargo.toml` o `Cargo.lock`, el script compila y vuelve a abrir la ventana de desarrollo. Si la compilación falla, mantiene abierta la última versión que pudo iniciar y muestra el error en la terminal. Pulsa `Ctrl+C` para detener la sesión.

La vista de desarrollo de **Tu Factura Ya Printer** usa ajustes y credenciales propios y mantiene desactivada la impresión automática. En macOS también permite probar el inicio al iniciar sesión con un registro independiente del de la aplicación instalada.

## Compilar

Se requiere Rust 1.95 o posterior. El proyecto versiona `Cargo.lock` para usar `--locked`.

```sh
cargo test --locked
cargo build --locked --release
```

Para empaquetar desde macOS:

```sh
MACOSX_DEPLOYMENT_TARGET=13.0 cargo build --locked --release
rustup target add x86_64-apple-darwin
MACOSX_DEPLOYMENT_TARGET=13.0 cargo build --locked --release --target x86_64-apple-darwin
./package.sh mac-universal
```

Para compilar Windows x64 desde macOS, instala Zig y `cargo-zigbuild` y ejecuta:

```sh
rustup target add x86_64-pc-windows-gnu
cargo zigbuild --locked --release --target x86_64-pc-windows-gnu
./package.sh windows-x64
```

Las builds de prueba son de firma ad hoc en macOS y portables sin firma Authenticode en Windows. `TFY_PRINTER_SIGN_IDENTITY` permite elegir una identidad de firma para el paquete macOS.
