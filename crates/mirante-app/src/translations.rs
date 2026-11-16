//! Module: mirante::translations
//! Purpose: Every interface string in Brazilian Portuguese, Greek and Spanish.
//! Created: 2026-11-16
//! Architecture: Keyed by the English text. The fixed-size array means a string
//! cannot be added without all three translations, or the build fails.

/// English source, then [Português (Brasil), Ελληνικά, Español].
pub const CATALOG: &[(&str, [&str; 3])] = &[
    ("Open…", ["Abrir…", "Άνοιγμα…", "Abrir…"]),
    ("Open", ["Abrir", "Άνοιγμα", "Abrir"]),
    ("Open Image", ["Abrir imagem", "Άνοιγμα εικόνας", "Abrir imagen"]),
    ("Cancel", ["Cancelar", "Ακύρωση", "Cancelar"]),
    ("Export", ["Exportar", "Εξαγωγή", "Exportar"]),
    ("Export…", ["Exportar…", "Εξαγωγή…", "Exportar…"]),
    ("Previous", ["Anterior", "Προηγούμενη", "Anterior"]),
    ("Next", ["Próxima", "Επόμενη", "Siguiente"]),
    ("Crop", ["Recortar", "Περικοπή", "Recortar"]),
    ("Rotate Left", ["Girar à esquerda", "Περιστροφή αριστερά", "Girar a la izquierda"]),
    ("Rotate Right", ["Girar à direita", "Περιστροφή δεξιά", "Girar a la derecha"]),
    (
        "Flip Horizontally",
        ["Espelhar horizontalmente", "Οριζόντιος κατοπτρισμός", "Voltear horizontalmente"],
    ),
    (
        "Flip Vertically",
        ["Espelhar verticalmente", "Κατακόρυφος κατοπτρισμός", "Voltear verticalmente"],
    ),
    ("Adjust", ["Ajustar", "Προσαρμογή", "Ajustar"]),
    ("Exposure", ["Exposição", "Έκθεση", "Exposición"]),
    ("Contrast", ["Contraste", "Αντίθεση", "Contraste"]),
    ("Saturation", ["Saturação", "Κορεσμός", "Saturación"]),
    ("Temperature", ["Temperatura", "Θερμοκρασία", "Temperatura"]),
    ("Tint", ["Matiz", "Απόχρωση", "Matiz"]),
    ("Reset", ["Redefinir", "Επαναφορά", "Restablecer"]),
    ("Revert to Original", ["Reverter ao original", "Επαναφορά στο αρχικό", "Volver al original"]),
    ("Display Profile", ["Perfil da tela", "Προφίλ οθόνης", "Perfil de pantalla"]),
    ("Current monitor", ["Monitor atual", "Τρέχουσα οθόνη", "Monitor actual"]),
    ("Load…", ["Carregar…", "Φόρτωση…", "Cargar…"]),
    ("ICC profiles", ["Perfis ICC", "Προφίλ ICC", "Perfiles ICC"]),
    ("All images", ["Todas as imagens", "Όλες οι εικόνες", "Todas las imágenes"]),
    ("Language", ["Idioma", "Γλώσσα", "Idioma"]),
    ("Menu", ["Menu", "Μενού", "Menú"]),
    ("Apply", ["Aplicar", "Εφαρμογή", "Aplicar"]),
    (
        "Could not open the image",
        [
            "Não foi possível abrir a imagem",
            "Δεν ήταν δυνατό το άνοιγμα της εικόνας",
            "No se pudo abrir la imagen",
        ],
    ),
    (
        "Could not export the image",
        [
            "Não foi possível exportar a imagem",
            "Δεν ήταν δυνατή η εξαγωγή της εικόνας",
            "No se pudo exportar la imagen",
        ],
    ),
    ("Settings", ["Configurações", "Ρυθμίσεις", "Configuración"]),
    ("Plugins", ["Plugins", "Πρόσθετα", "Complementos"]),
    ("Install…", ["Instalar…", "Εγκατάσταση…", "Instalar…"]),
    ("Remove", ["Remover", "Αφαίρεση", "Quitar"]),
    ("Restore", ["Restaurar", "Επαναφορά", "Restaurar"]),
    ("Just me", ["Só para mim", "Μόνο για μένα", "Solo para mí"]),
    ("Everyone", ["Todos os usuários", "Όλοι οι χρήστες", "Todos los usuarios"]),
    ("For me", ["Para mim", "Για μένα", "Para mí"]),
    ("For everyone", ["Para todos", "Για όλους", "Para todos"]),
    (
        "Install this plugin?",
        [
            "Instalar este plugin?",
            "Εγκατάσταση αυτού του πρόσθετου;",
            "¿Instalar este complemento?",
        ],
    ),
    (
        "Remove this plugin?",
        ["Remover este plugin?", "Αφαίρεση αυτού του πρόσθετου;", "¿Quitar este complemento?"],
    ),
    ("Mirante plugins", ["Plugins do Mirante", "Πρόσθετα Mirante", "Complementos de Mirante"]),
    (
        "Could not install the plugin",
        [
            "Não foi possível instalar o plugin",
            "Δεν ήταν δυνατή η εγκατάσταση του πρόσθετου",
            "No se pudo instalar el complemento",
        ],
    ),
    (
        "Could not remove the plugin",
        [
            "Não foi possível remover o plugin",
            "Δεν ήταν δυνατή η αφαίρεση του πρόσθετου",
            "No se pudo quitar el complemento",
        ],
    ),
    (
        "Choose another name: the original is never changed.",
        [
            "Escolha outro nome: o original nunca é alterado.",
            "Επιλέξτε άλλο όνομα: το πρωτότυπο δεν αλλάζει ποτέ.",
            "Elija otro nombre: el original nunca se modifica.",
        ],
    ),
];
