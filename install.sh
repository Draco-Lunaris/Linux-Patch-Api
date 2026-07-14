#!/bin/bash
# Linux Patch API - Interactive Installation Script
# For manual deployment on systems without package manager
# Supports Debian/Ubuntu, RHEL/CentOS/Fedora, Alpine, Arch

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Configuration
APP_NAME="linux-patch-api"
VERSION="2.2.0"
INSTALL_DIR="/usr/bin"
CONFIG_DIR="/etc/linux_patch_api"
CERTS_DIR="${CONFIG_DIR}/certs"
DATA_DIR="/var/lib/linux_patch_api"
LOG_DIR="/var/log/linux_patch_api"
SERVICE_FILE="/lib/systemd/system/linux-patch-api.service"
SYSTEM_USER="linux-patch-api"
SYSTEM_GROUP="linux-patch-api"

# Logging functions
log_info() {
    echo -e "${BLUE}[INFO]${NC} $1"
}

log_success() {
    echo -e "${GREEN}[SUCCESS]${NC} $1"
}

log_warning() {
    echo -e "${YELLOW}[WARNING]${NC} $1"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

# Check if running as root
check_root() {
    if [ "$(id -u)" -ne 0 ]; then
        log_error "This script must be run as root"
        exit 1
    fi
    log_info "Running as root - OK"
}

# Detect OS and package manager
detect_os() {
    if [ -f /etc/os-release ]; then
        . /etc/os-release
        OS_ID=$ID
        OS_VERSION=$VERSION_ID
        log_info "Detected OS: ${OS_ID} ${OS_VERSION}"
    else
        log_error "Cannot detect operating system"
        exit 1
    fi
}

# Check prerequisites
check_prerequisites() {
    log_info "Checking prerequisites..."
    
    # Check for systemd
    if ! command -v systemctl &> /dev/null; then
        log_error "systemd is required but not found"
        exit 1
    fi
    log_info "  - systemd: OK"
    
    # Check for required directories
    log_info "  - Checking directory structure..."
    
    # Check if binary exists
    if [ -f "target/x86_64-unknown-linux-gnu/release/${APP_NAME}" ]; then
        BINARY_PATH="target/x86_64-unknown-linux-gnu/release/${APP_NAME}"
        log_info "  - Binary found: ${BINARY_PATH}"
    elif [ -f "target/release/${APP_NAME}" ]; then
        BINARY_PATH="target/release/${APP_NAME}"
        log_info "  - Binary found: ${BINARY_PATH}"
    else
        log_error "Binary not found. Please build first with: cargo build --release"
        exit 1
    fi
    
    log_success "Prerequisites check passed"
}

# Create system user and group
create_system_user() {
    log_info "Creating system user and group..."
    
    # Create group if it doesn't exist
    if ! getent group ${SYSTEM_GROUP} > /dev/null 2>&1; then
        groupadd --system ${SYSTEM_GROUP}
        log_info "  - Created group: ${SYSTEM_GROUP}"
    else
        log_info "  - Group already exists: ${SYSTEM_GROUP}"
    fi
    
    # Create user if it doesn't exist
    if ! getent passwd ${SYSTEM_USER} > /dev/null 2>&1; then
        useradd --system \
            --gid ${SYSTEM_GROUP} \
            --home-dir ${DATA_DIR} \
            --no-create-home \
            --shell /usr/sbin/nologin \
            --comment "Linux Patch API Service" \
            ${SYSTEM_USER}
        log_info "  - Created user: ${SYSTEM_USER}"
    else
        log_info "  - User already exists: ${SYSTEM_USER}"
    fi
}

# Create directory structure
create_directories() {
    log_info "Creating directory structure..."
    
    mkdir -p ${CONFIG_DIR}
    mkdir -p ${CERTS_DIR}
    mkdir -p ${DATA_DIR}
    mkdir -p ${LOG_DIR}
    
    # Set ownership
    chown -R ${SYSTEM_USER}:${SYSTEM_GROUP} ${DATA_DIR}
    chown -R ${SYSTEM_USER}:${SYSTEM_GROUP} ${LOG_DIR}
    chown -R ${SYSTEM_USER}:${SYSTEM_GROUP} ${CONFIG_DIR}
    
    # Set permissions
    chmod 750 ${CONFIG_DIR}
    chmod 750 ${CERTS_DIR}
    chmod 755 ${DATA_DIR}
    chmod 755 ${LOG_DIR}
    
    log_success "Directory structure created"
}

# Install binary
install_binary() {
    log_info "Installing binary to ${INSTALL_DIR}..."
    
    cp ${BINARY_PATH} ${INSTALL_DIR}/${APP_NAME}
    chmod 755 ${INSTALL_DIR}/${APP_NAME}
    
    log_success "Binary installed: ${INSTALL_DIR}/${APP_NAME}"
}

# Install configuration files
install_config() {
    log_info "Installing configuration files..."
    
    # Copy example configs if they don't exist
    if [ ! -f "${CONFIG_DIR}/config.yaml" ]; then
        if [ -f "configs/config.yaml.example" ]; then
            cp configs/config.yaml.example ${CONFIG_DIR}/config.yaml
            chmod 640 ${CONFIG_DIR}/config.yaml
            log_info "  - Created default config.yaml"
        else
            log_warning "  - config.yaml.example not found"
        fi
    else
        log_info "  - config.yaml already exists"
    fi
    
    if [ ! -f "${CONFIG_DIR}/whitelist.yaml" ]; then
        if [ -f "configs/whitelist.yaml.example" ]; then
            cp configs/whitelist.yaml.example ${CONFIG_DIR}/whitelist.yaml
            chmod 640 ${CONFIG_DIR}/whitelist.yaml
            log_info "  - Created default whitelist.yaml"
        else
            log_warning "  - whitelist.yaml.example not found"
        fi
    else
        log_info "  - whitelist.yaml already exists"
    fi
    
    log_success "Configuration files installed"
}

# Install systemd service
install_service() {
    log_info "Installing systemd service..."
    
    if [ -f "configs/linux-patch-api.service" ]; then
        cp configs/linux-patch-api.service ${SERVICE_FILE}
        chmod 644 ${SERVICE_FILE}
        systemctl daemon-reload
        log_success "Systemd service installed"
    else
        log_error "Service file not found: configs/linux-patch-api.service"
        exit 1
    fi
}

# Generate self-signed certificates (optional)
generate_certificates() {
    log_info "Checking for TLS certificates..."
    
    if [ ! -f "${CERTS_DIR}/server.pem" ] || [ ! -f "${CERTS_DIR}/server.key.pem" ]; then
        echo ""
        echo -e "${YELLOW}No TLS certificates found.${NC}"
        read -p "Generate self-signed certificates for testing? (y/n): " -n 1 -r
        echo ""
        
        if [[ $REPLY =~ ^[Yy]$ ]]; then
            log_info "Generating self-signed certificates..."
            
            # Generate CA
            openssl genrsa -out ${CERTS_DIR}/ca.key.pem 4096
            openssl req -x509 -new -nodes -sha256 -days 3650 \
                -key ${CERTS_DIR}/ca.key.pem \
                -out ${CERTS_DIR}/ca.pem \
                -subj "/CN=Linux Patch API CA"
            
            # Generate server key and CSR
            openssl genrsa -out ${CERTS_DIR}/server.key.pem 2048
            openssl req -new \
                -key ${CERTS_DIR}/server.key.pem \
                -out ${CERTS_DIR}/server.csr.pem \
                -subj "/CN=localhost"
            
            # Sign server certificate
            openssl x509 -req -sha256 -days 365 \
                -in ${CERTS_DIR}/server.csr.pem \
                -CA ${CERTS_DIR}/ca.pem \
                -CAkey ${CERTS_DIR}/ca.key.pem \
                -CAcreateserial \
                -out ${CERTS_DIR}/server.pem
            
            # Set secure permissions
            chmod 600 ${CERTS_DIR}/*.key.pem
            chmod 644 ${CERTS_DIR}/*.pem
            chown -R ${SYSTEM_USER}:${SYSTEM_GROUP} ${CERTS_DIR}
            
            log_success "Self-signed certificates generated"
            log_warning "NOTE: Self-signed certificates are for testing only. Use proper CA-signed certs in production."
        else
            log_warning "Skipping certificate generation. Please place certificates in ${CERTS_DIR} manually."
        fi
    else
        log_info "  - Certificates already exist"
    fi
}

# Enable and start service
enable_service() {
    echo ""
    read -p "Enable and start the service now? (y/n): " -n 1 -r
    echo ""
    
    if [[ $REPLY =~ ^[Yy]$ ]]; then
        log_info "Enabling service..."
        systemctl enable ${APP_NAME}.service
        
        log_info "Starting service..."
        systemctl start ${APP_NAME}.service
        
        # Check service status
        if systemctl is-active --quiet ${APP_NAME}.service; then
            log_success "Service started successfully"
            systemctl status ${APP_NAME}.service --no-pager
        else
            log_error "Service failed to start. Check logs: journalctl -u ${APP_NAME}"
            exit 1
        fi
    else
        log_info "Service not started. You can start it later with: systemctl start ${APP_NAME}"
    fi
}

# Display installation summary
display_summary() {
    echo ""
    echo -e "${GREEN}========================================${NC}"
    echo -e "${GREEN}  Linux Patch API Installation Complete${NC}"
    echo -e "${GREEN}========================================${NC}"
    echo ""
    echo -e "Version:        ${VERSION}"
    echo -e "Binary:         ${INSTALL_DIR}/${APP_NAME}"
    echo -e "Config:         ${CONFIG_DIR}/config.yaml"
    echo -e "Whitelist:      ${CONFIG_DIR}/whitelist.yaml"
    echo -e "Certificates:   ${CERTS_DIR}/"
    echo -e "Data:           ${DATA_DIR}/"
    echo -e "Logs:           ${LOG_DIR}/"
    echo -e "Service:        ${APP_NAME}.service"
    echo ""
    echo -e "${YELLOW}Next Steps:${NC}"
    echo "  1. Review and configure ${CONFIG_DIR}/config.yaml"
    echo "  2. Configure IP whitelist in ${CONFIG_DIR}/whitelist.yaml"
    echo "  3. Replace self-signed certificates with CA-signed certs for production"
    echo "  4. Start service: systemctl start ${APP_NAME}"
    echo "  5. Check status: systemctl status ${APP_NAME}"
    echo "  6. View logs: journalctl -u ${APP_NAME} -f"
    echo ""
    echo -e "API Endpoint:   https://localhost:12443/api/v1/"
    echo ""
}

# Main installation flow
main() {
    echo ""
    echo -e "${BLUE}========================================${NC}"
    echo -e "${BLUE}  Linux Patch API Installer v${VERSION}${NC}"
    echo -e "${BLUE}========================================${NC}"
    echo ""
    
    check_root
    detect_os
    check_prerequisites
    create_system_user
    create_directories
    install_binary
    install_config
    install_service
    generate_certificates
    enable_service
    display_summary
    
    log_success "Installation completed successfully!"
}

# Run main function
main "$@"